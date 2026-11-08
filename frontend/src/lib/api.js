import axios from 'axios';
import { runtime } from './runtime';

export const api = axios.create({
  baseURL: runtime.apiBaseUrl,
  timeout: 30_000,
  headers: runtime.authToken ? { 'X-YTU-Token': runtime.authToken } : {},
});

export function getApiError(error, fallback = 'Ocurrió un error inesperado') {
  const detail = error?.response?.data?.detail;
  if (typeof detail === 'string' && detail.trim()) return detail;
  if (Array.isArray(detail) && detail[0]?.msg) return detail[0].msg;
  if (error?.code === 'ECONNABORTED') return 'La operación tardó demasiado';
  if (!error?.response && error?.message) return 'No se pudo conectar con el servicio local';
  return fallback;
}
