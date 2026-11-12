import { useCallback } from 'react';
import toast from 'react-hot-toast';
import { api, getApiError } from '../lib/api';
import { chooseMediaFiles } from '../lib/desktop';

export function useMediaImport() {
  const importPaths = useCallback(async (paths, options = {}) => {
    const uniquePaths = [...new Set((paths || []).filter(Boolean))];
    if (!uniquePaths.length) return null;

    const toastId = toast.loading(
      uniquePaths.length === 1 ? 'Importando archivo…' : 'Importando archivos…',
    );
    try {
      const { data } = await api.post('/library/import', {
        paths: uniquePaths,
        mode: options.mode || 'copy',
        playlist_name: options.playlistName || '',
      });
      const imported = data.imported?.length ?? data.imported_count ?? 0;
      const skipped = data.skipped?.length ?? data.skipped_count ?? 0;
      toast.success(
        imported
          ? `${imported} ${imported === 1 ? 'archivo importado' : 'archivos importados'}`
          : skipped
            ? 'Esos archivos ya estaban en tu biblioteca'
            : 'Importación completada',
        { id: toastId },
      );
      window.dispatchEvent(new CustomEvent('ytu:library-changed', { detail: data }));
      return data;
    } catch (error) {
      toast.error(getApiError(error, 'No se pudieron importar los archivos'), { id: toastId });
      return null;
    }
  }, []);

  const chooseAndImport = useCallback(async (kind = 'files', options = {}) => {
    const paths = await chooseMediaFiles({ kind });
    return importPaths(paths, options);
  }, [importPaths]);

  return { importPaths, chooseAndImport };
}
