import React, { useState, useEffect } from 'react';
import toast from 'react-hot-toast';
import { api } from '../lib/api';
import {
  Trash2, Download, CheckCircle2, AlertCircle,
  Loader2, DownloadCloud, Music, Video, ListMusic,
} from 'lucide-react';
import { Thumb, EmptyState } from './ui';

const STATUS_MAP = {
  pending:     { icon: Loader2,      tone: 'muted',   label: 'En cola',        spin: true  },
  starting:    { icon: Loader2,      tone: 'muted',   label: 'Iniciando…',     spin: true  },
  downloading: { icon: Download,     tone: 'accent',  label: 'Descargando',    spin: false },
  merging:     { icon: Loader2,      tone: 'muted',   label: 'Combinando…',    spin: true  },
  converting:  { icon: Loader2,      tone: 'muted',   label: 'Convirtiendo…',  spin: true  },
  processing:  { icon: Loader2,      tone: 'muted',   label: 'Procesando…',    spin: true  },
  completed:   { icon: CheckCircle2, tone: 'success', label: 'Completado',     spin: false },
  error:       { icon: AlertCircle,  tone: 'error',   label: 'Error',          spin: false },
  canceled:    { icon: AlertCircle,  tone: 'muted',   label: 'Cancelado',      spin: false },
};

const TONE_COLORS = {
  muted: 'var(--text-tertiary)',
  accent: 'var(--accent)',
  success: 'var(--system-green)',
  error: 'var(--system-red)',
};

export default function Downloads({ active }) {
  const [items, setItems] = useState([]);

  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    let fetching = false;

    const fetchDownloads = async () => {
      if (fetching || !alive) return;
      fetching = true;
      try {
        const { data } = await api.get('/downloads');
        if (alive) setItems([...data].reverse());
      } catch {
        // ignore
      }
      fetching = false;
    };

    fetchDownloads();
    const iv = setInterval(fetchDownloads, 2000);
    return () => { alive = false; clearInterval(iv); };
  }, [active]);

  const clear = async (id) => {
    await api.delete(`/downloads/${id}`);
    setItems((p) => p.filter((i) => i.id !== id));
  };

  const clearDone = async () => {
    await api.post('/downloads/clear_completed');
    setItems((p) => p.filter((i) => !['completed', 'error', 'canceled'].includes(i.status)));
    toast.success('Historial limpio');
  };

  const inProgress = items.filter((i) => !['completed', 'error', 'canceled'].includes(i.status));
  const done = items.filter((i) => i.status === 'completed');
  const errors = items.filter((i) => ['error', 'canceled'].includes(i.status));

  const renderItem = (item, showProgressBar) => {
    const st = STATUS_MAP[item.status] || STATUS_MAP.pending;
    const Icon = st.icon;
    const color = TONE_COLORS[st.tone];
    const showProgress = showProgressBar && item.progress > 0 && !['completed', 'error'].includes(item.status);

    return (
      <div key={item.id} className="task-row">
        <Thumb
          src={item.thumbnail}
          size="md"
          icon={item.is_playlist ? ListMusic : item.type === 'audio' ? Music : Video}
          iconSize={16}
        >
          {item.is_playlist && (
            <div className="playlist-badge">
              {item.downloaded || `${item.playlist_total}`}
            </div>
          )}
        </Thumb>

        <div className="task-row__body">
          <div className="task-row__title">{item.title}</div>
          <div className="task-row__status">
            <Icon size={12} className={st.spin ? 'spinner' : ''} style={{ color, flexShrink: 0 }} />
            <span style={{ color }}>{st.label}</span>
            {item.speed && item.status === 'downloading' && <span>· {item.speed}</span>}
            {item.eta && item.status === 'downloading' && <span>· quedan {item.eta}</span>}
          </div>
          {showProgress && (
            <div className="progress-track">
              <div className="progress-fill" style={{ width: `${item.progress || 0}%` }} />
            </div>
          )}
          {item.error && <div className="task-row__error">{item.error}</div>}
        </div>

        {showProgress && (
          <span className="task-row__pct">{Math.round(item.progress)}%</span>
        )}

        <button onClick={() => clear(item.id)} className="btn-icon-sm" title="Quitar de la lista">
          <Trash2 size={13} />
        </button>
      </div>
    );
  };

  return (
    <div className="page-shell">
      <div className="toolbar">
        <div className="toolbar__left">
          <h1 className="page-title">Descargas</h1>
          {inProgress.length > 0 && <span className="page-count">{inProgress.length} en curso</span>}
        </div>
        {(done.length > 0 || errors.length > 0) && (
          <div className="toolbar__right">
            <button onClick={clearDone} className="btn-secondary">
              <Trash2 size={13} /> Limpiar historial
            </button>
          </div>
        )}
      </div>

      {!items.length && (
        <EmptyState
          icon={DownloadCloud}
          title="No hay descargas"
          hint="Desde Buscar puedes guardar cualquier canción o vídeo; el progreso aparecerá aquí."
        />
      )}

      {inProgress.length > 0 && (
        <div className="section">
          <div className="section-header">
            <h2 className="section-title">En curso</h2>
            <span className="section-count">{inProgress.length}</span>
          </div>
          {inProgress.map((item) => renderItem(item, true))}
        </div>
      )}

      {done.length > 0 && (
        <div className="section">
          <div className="section-header">
            <h2 className="section-title">Completadas</h2>
            <span className="section-count">{done.length}</span>
          </div>
          {done.map((item) => renderItem(item, false))}
        </div>
      )}

      {errors.length > 0 && (
        <div className="section">
          <div className="section-header">
            <h2 className="section-title" style={{ color: 'var(--system-red)' }}>Con errores</h2>
          </div>
          {errors.map((item) => renderItem(item, false))}
        </div>
      )}
    </div>
  );
}
