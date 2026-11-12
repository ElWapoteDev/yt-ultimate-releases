import toast from 'react-hot-toast';
import { copyText, getDesktopBridge, openWithDefaultApp, revealFile } from './desktop';
import { fileManagerName } from './platform';

// Menu entries every song menu shares; pages add their own around them.
export function trackPlaybackItems(file, { play, playNext, playLater }) {
  return [
    { label: 'Reproducir', onSelect: () => play(file) },
    {
      label: 'Reproducir a continuación',
      onSelect: () => {
        playNext(file);
        toast.success('Se reproducirá a continuación');
      },
    },
    {
      label: 'Agregar a la cola',
      onSelect: () => {
        playLater(file);
        toast.success('Agregado a la cola');
      },
    },
  ];
}

export function trackFileItems(file) {
  const visible = Boolean(getDesktopBridge());
  const local = Boolean(file?.local_path);
  return [
    {
      label: `Mostrar en ${fileManagerName}`,
      visible,
      enabled: local,
      onSelect: () => void revealFile(file.local_path),
    },
    {
      label: 'Abrir con la app predeterminada',
      visible,
      enabled: local,
      onSelect: () => void openWithDefaultApp(file.local_path),
    },
    {
      label: 'Copiar ruta',
      visible,
      enabled: local,
      onSelect: () => copyText(file.local_path),
    },
  ];
}

export const CHIPMUNK_EVENT = 'ytu:chipmunk-open';

// Opens ChipmunkDialog for a local song (audio or video).
export function trackChipmunkItems(file) {
  const local = Boolean(file?.local_path || file?.id) && (file?.type === 'audio' || file?.type === 'video');
  return [
    {
      label: 'Versión Alvin y las ardillas…',
      visible: Boolean(file) && !file.is_stream,
      enabled: local,
      onSelect: () => window.dispatchEvent(new CustomEvent(CHIPMUNK_EVENT, { detail: file })),
    },
  ];
}
