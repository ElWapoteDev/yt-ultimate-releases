import React, { useEffect, useRef, useState } from 'react';
import { FileMusic, FolderInput } from 'lucide-react';
import { endOutgoingDrag, isOutgoingDrag, pathsFromDroppedFiles } from '../lib/desktop';

function containsFiles(event) {
  return Array.from(event.dataTransfer?.types || []).includes('Files');
}

export default function ImportDropZone({ onImport }) {
  const [visible, setVisible] = useState(false);
  const dragDepth = useRef(0);

  useEffect(() => {
    // Songs dragged out of the library start over this window; they neither
    // show the import overlay nor drop back in.
    const onDragEnter = (event) => {
      if (!containsFiles(event)) return;
      event.preventDefault();
      dragDepth.current += 1;
      if (!isOutgoingDrag()) setVisible(true);
    };
    const onDragOver = (event) => {
      if (!containsFiles(event)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = isOutgoingDrag() ? 'none' : 'copy';
    };
    const onDragLeave = (event) => {
      if (!containsFiles(event)) return;
      event.preventDefault();
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (dragDepth.current === 0) {
        setVisible(false);
        // Once the song left the window, a drag coming in is a new one.
        endOutgoingDrag();
      }
    };
    const onDrop = (event) => {
      event.preventDefault();
      dragDepth.current = 0;
      setVisible(false);
      if (isOutgoingDrag()) {
        endOutgoingDrag();
        return;
      }
      const paths = pathsFromDroppedFiles(event.dataTransfer?.files);
      if (paths.length) void onImport(paths);
    };
    const onPointerDown = () => endOutgoingDrag();
    const onPointerMove = (event) => {
      if (event.buttons === 0) endOutgoingDrag();
    };

    window.addEventListener('dragenter', onDragEnter);
    window.addEventListener('dragover', onDragOver);
    window.addEventListener('dragleave', onDragLeave);
    window.addEventListener('drop', onDrop);
    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('pointermove', onPointerMove, true);
    return () => {
      window.removeEventListener('dragenter', onDragEnter);
      window.removeEventListener('dragover', onDragOver);
      window.removeEventListener('dragleave', onDragLeave);
      window.removeEventListener('drop', onDrop);
      window.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('pointermove', onPointerMove, true);
    };
  }, [onImport]);

  if (!visible) return null;

  return (
    <div className="import-drop-zone" role="status" aria-live="polite">
      <div className="import-drop-zone__card">
        <div className="import-drop-zone__icon">
          <FolderInput size={28} />
          <FileMusic size={17} />
        </div>
        <div className="import-drop-zone__title">Añadir a tu biblioteca</div>
        <div className="import-drop-zone__hint">
          Suelta canciones, vídeos, carpetas o playlists M3U
        </div>
      </div>
    </div>
  );
}
