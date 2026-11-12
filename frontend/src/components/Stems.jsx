import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../lib/api';
import toast from 'react-hot-toast';
import { resourceUrl } from '../lib/runtime';
import { deviceName } from '../lib/platform';
import { thumbUrl } from '../lib/format';
import {
  AlertCircle, CheckCircle2, Download, Loader2, Music, Scissors,
  Search as SearchIcon, X,
} from 'lucide-react';
import { Thumb, EmptyState } from './ui';

const STATUS = {
  pending: { icon: Loader2, label: 'En cola', tone: 'muted', spin: true },
  installing: { icon: Loader2, label: 'Instalando modelo', tone: 'accent', spin: true },
  processing: { icon: Loader2, label: 'Separando', tone: 'accent', spin: true },
  completed: { icon: CheckCircle2, label: 'Completado', tone: 'success', spin: false },
  error: { icon: AlertCircle, label: 'Error', tone: 'error', spin: false },
  canceled: { icon: X, label: 'Cancelado', tone: 'muted', spin: false },
};

const TONES = {
  muted: 'var(--text-tertiary)',
  accent: 'var(--accent)',
  success: 'var(--system-green)',
  error: 'var(--system-red)',
};

function formatBytes(bytes) {
  if (!bytes) return '0 MB';
  if (bytes >= 1073741824) return `${(bytes / 1073741824).toFixed(1)} GB`;
  return `${(bytes / 1048576).toFixed(0)} MB`;
}

export default function Stems({ active }) {
  const [files, setFiles] = useState([]);
  const [jobs, setJobs] = useState([]);
  const [info, setInfo] = useState(null);
  const [selected, setSelected] = useState(null);
  const [query, setQuery] = useState('');
  const [format, setFormat] = useState('FLAC');
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    (async () => {
      try {
        const [libraryResponse, jobsResponse, infoResponse] = await Promise.all([
          api.get('/library'),
          api.get('/separator/jobs'),
          api.get('/separator/info'),
        ]);
        if (!alive) return;
        setFiles(libraryResponse.data);
        setJobs(jobsResponse.data);
        setInfo(infoResponse.data);
      } catch {
        // backend not ready
      }
    })();
    return () => { alive = false; };
  }, [active]);

  // Poll job progress only while the tab is visible
  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    const iv = setInterval(async () => {
      if (!alive) return;
      try {
        const { data } = await api.get('/separator/jobs');
        if (alive) setJobs(data);
      } catch {
        // ignore
      }
    }, 1500);
    return () => { alive = false; clearInterval(iv); };
  }, [active]);

  const sourceFiles = useMemo(() => {
    const q = query.trim().toLowerCase();
    return files
      .filter((file) => (file.type === 'audio' || file.type === 'video') && file.playlist !== 'Separaciones')
      .filter((file) => !q || `${file.name} ${file.channel} ${file.filename}`.toLowerCase().includes(q))
      .slice(0, 60);
  }, [files, query]);

  const activeJobs = jobs.filter((job) => ['pending', 'installing', 'processing'].includes(job.status));
  const recentJobs = jobs.filter((job) => !['pending', 'installing', 'processing'].includes(job.status)).slice(0, 8);

  const start = async () => {
    if (!selected) return;
    setStarting(true);
    try {
      const { data } = await api.post('/separator/jobs', {
        track_id: selected.id || '',
        filename: selected.filename,
        local_path: selected.local_path,
        title: selected.name,
        output_format: format,
      });
      setJobs((prev) => [data, ...prev]);
      toast.success('Separación iniciada');
    } catch (e) {
      toast.error(e.response?.data?.detail || 'No se pudo iniciar');
    }
    setStarting(false);
  };

  const cancelJob = async (id) => {
    await api.delete(`/separator/jobs/${id}`);
    try {
      const { data } = await api.get('/separator/jobs');
      setJobs(data);
    } catch {
      // ignore
    }
  };

  return (
    <div className="page-shell">
      <div className="toolbar">
        <div className="toolbar__left">
          <h1 className="page-title">Stems</h1>
          <span className="page-count">{info?.model_label || 'Modelo local'}</span>
        </div>
      </div>
      <div className="page-subtitle" style={{ marginTop: -14, marginBottom: 22 }}>
        Separa la voz y el instrumental de cualquier canción, directamente en {deviceName}.
      </div>

      <div className="stems-layout">
        <div className="stems-main">
          <div className="input-search-wrap stems-search">
            <SearchIcon className="input-search-icon" size={14} />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="input-search"
              placeholder="Buscar en tu biblioteca"
            />
            {query && (
              <button onClick={() => setQuery('')} className="input-search-clear" aria-label="Limpiar">
                ×
              </button>
            )}
          </div>

          {!sourceFiles.length && (
            <EmptyState
              icon={Music}
              title="No hay canciones para separar"
              hint="Descarga audio o vídeo desde Buscar y aparecerá aquí."
              compact
            />
          )}

          {sourceFiles.map((file) => {
            const isSel = selected?.local_path === file.local_path;
            return (
              <button
                key={file.local_path || file.filename}
                onClick={() => setSelected(isSel ? null : file)}
                className={`row ${isSel ? 'row--active' : ''}`}
              >
                <Thumb src={thumbUrl(file)} size="sm" />
                <div className="row__meta">
                  <div className={`row__title ${isSel ? 'row__title--active' : ''}`}>{file.name}</div>
                  <div className="row__sub">{file.channel || file.filename}</div>
                </div>
                <span className="row__detail">{file.size}</span>
              </button>
            );
          })}
        </div>

        <div className="stems-side">
          <div className="settings-section" style={{ marginBottom: 0 }}>
            <div className="settings-section__header">
              <div className="settings-section__icon"><Scissors size={15} /></div>
              <div>
                <div className="settings-section__title">Extracción local</div>
                <div className="settings-section__sub">
                  {info?.model_ready ? `Modelo listo · ${formatBytes(info.model_size_bytes)}` : 'El modelo se instala al primer uso'}
                </div>
              </div>
            </div>

            <div className="stems-control">
              <div className="stems-selected">
                <span className="stems-selected__label">Canción seleccionada</span>
                <span className="stems-selected__title" style={!selected ? { color: 'var(--text-tertiary)', fontWeight: 500 } : undefined}>
                  {selected?.name || 'Elige una de la lista'}
                </span>
              </div>

              <div className="segmented stems-format">
                {(info?.output_formats || ['FLAC', 'MP3', 'WAV']).map((fmt) => (
                  <button
                    key={fmt}
                    onClick={() => setFormat(fmt)}
                    className={`segment ${format === fmt ? 'active' : ''}`}
                  >
                    {fmt}
                  </button>
                ))}
              </div>

              <button
                onClick={start}
                className="btn-accent stems-start"
                disabled={!selected || starting}
              >
                {starting ? <Loader2 size={13} className="spinner" /> : <Scissors size={13} />}
                Separar
              </button>
            </div>
          </div>

          {(activeJobs.length > 0 || recentJobs.length > 0) && (
            <div>
              <div className="section-header">
                <h2 className="section-title" style={{ fontSize: 14 }}>Trabajos</h2>
              </div>
              {[...activeJobs, ...recentJobs].map((job) => (
                <JobCard key={job.id} job={job} onCancel={cancelJob} />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function JobCard({ job, onCancel }) {
  const st = STATUS[job.status] || STATUS.pending;
  const Icon = st.icon;
  const color = TONES[st.tone];
  const isActive = ['pending', 'installing', 'processing'].includes(job.status);

  return (
    <div className="task-row" style={{ alignItems: 'flex-start' }}>
      <div className="task-row__body">
        <div className="task-row__title">{job.title}</div>
        <div className="task-row__status">
          <Icon size={12} className={st.spin ? 'spinner' : ''} style={{ color, flexShrink: 0 }} />
          <span style={{ color }}>{st.label}</span>
          <span>· {job.stage || job.output_format}</span>
        </div>
        {isActive && (
          <div className="progress-track">
            <div className="progress-fill" style={{ width: `${job.progress || 0}%` }} />
          </div>
        )}
        {job.error && <div className="task-row__error">{job.error}</div>}
        {job.outputs?.length > 0 && (
          <div className="stems-output-list">
            {job.outputs.map((output) => (
              <a
                key={output.filename}
                className="stems-output"
                href={resourceUrl(output.path)}
                download
              >
                <Download size={11} />
                {output.label}
              </a>
            ))}
          </div>
        )}
      </div>
      {isActive && (
        <>
          <span className="task-row__pct">{Math.round(job.progress || 0)}%</span>
          <button onClick={() => onCancel(job.id)} className="btn-icon-sm" title="Cancelar">
            <X size={13} />
          </button>
        </>
      )}
    </div>
  );
}
