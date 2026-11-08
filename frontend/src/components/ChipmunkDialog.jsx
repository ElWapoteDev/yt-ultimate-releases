import React, { useCallback, useEffect, useState } from 'react';
import toast from 'react-hot-toast';
import { X } from 'lucide-react';
import { api, getApiError } from '../lib/api';
import { CHIPMUNK_EVENT } from '../lib/trackMenu';
import { thumbUrl } from '../lib/format';
import { Thumb } from './ui';

const CHIPMUNK_PRESETS = [
  { id: 'classic', label: 'Clásica', pitch: 7, speed: 1, brightness: 3 },
  { id: 'soft', label: 'Suave', pitch: 4, speed: 1, brightness: 2 },
  { id: 'super', label: 'Súper aguda', pitch: 12, speed: 1, brightness: 5 },
  { id: 'fast', label: 'Acelerada', pitch: 9, speed: 1.25, brightness: 4 },
];

const DEFAULTS = CHIPMUNK_PRESETS[0];
const POLL_MS = 1500;
const TERMINAL = new Set(['completed', 'error', 'canceled']);

function formatSemitones(value) {
  return `+${Number(value).toFixed(value % 1 ? 1 : 0)} semitonos`;
}

async function followJob(jobId, title) {
  const toastId = `chipmunk-${jobId}`;
  toast.loading(`Creando versión ardilla de "${title}"…`, { id: toastId });
  for (;;) {
    await new Promise((resolve) => setTimeout(resolve, POLL_MS));
    let job;
    try {
      ({ data: job } = await api.get(`/chipmunk/jobs/${encodeURIComponent(jobId)}`));
    } catch {
      continue;
    }
    if (!TERMINAL.has(job.status)) {
      const progress = Math.round(Number(job.progress) || 0);
      toast.loading(`Creando versión ardilla de "${title}"… ${progress}%`, { id: toastId });
      continue;
    }
    if (job.status === 'completed') {
      window.dispatchEvent(new CustomEvent('ytu:library-changed', { detail: job.output || null }));
      toast.success(`Lista: "${title} (Ardilla)". Está en Canciones.`, { id: toastId, duration: 5000 });
    } else if (job.status === 'error') {
      toast.error(job.error || 'No se pudo crear la versión ardilla', { id: toastId });
    } else {
      toast.dismiss(toastId);
    }
    return;
  }
}

function Slider({ label, value, display, min, max, step, onChange }) {
  return (
    <label className="chipmunk-slider">
      <span className="chipmunk-slider__head">
        <span>{label}</span>
        <span className="chipmunk-slider__value">{display}</span>
      </span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </label>
  );
}

// Opened from any song menu ("Versión Alvin y las ardillas…").
export default function ChipmunkDialog() {
  const [file, setFile] = useState(null);
  const [pitch, setPitch] = useState(DEFAULTS.pitch);
  const [speed, setSpeed] = useState(DEFAULTS.speed);
  const [brightness, setBrightness] = useState(DEFAULTS.brightness);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    const open = (event) => {
      if (event.detail) setFile(event.detail);
    };
    window.addEventListener(CHIPMUNK_EVENT, open);
    return () => window.removeEventListener(CHIPMUNK_EVENT, open);
  }, []);

  useEffect(() => {
    if (!file) return undefined;
    const onKey = (event) => {
      if (event.key === 'Escape') setFile(null);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [file]);

  const applyPreset = (preset) => {
    setPitch(preset.pitch);
    setSpeed(preset.speed);
    setBrightness(preset.brightness);
  };

  const submit = useCallback(async () => {
    if (!file || submitting) return;
    setSubmitting(true);
    const title = file.name || file.filename;
    try {
      const { data } = await api.post('/chipmunk/jobs', {
        track_id: file.id || '',
        filename: file.filename || '',
        local_path: file.local_path || '',
        title,
        pitch_semitones: pitch,
        speed,
        brightness_db: brightness,
      });
      setFile(null);
      void followJob(data.id, title);
    } catch (error) {
      toast.error(getApiError(error, 'No se pudo iniciar la versión ardilla'));
    } finally {
      setSubmitting(false);
    }
  }, [brightness, file, pitch, speed, submitting]);

  if (!file) return null;

  const activePreset = CHIPMUNK_PRESETS.find((preset) => (
    preset.pitch === pitch && preset.speed === speed && preset.brightness === brightness
  ));

  return (
    <div className="modal-backdrop" onClick={() => setFile(null)}>
      <div
        className="modal-panel chipmunk-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Versión Alvin y las ardillas"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-header">
          <div style={{ display: 'flex', gap: 14, minWidth: 0, flex: 1 }}>
            <Thumb src={thumbUrl(file)} size="lg" iconSize={20} />
            <div style={{ minWidth: 0, flex: 1 }}>
              <div className="modal-title">Versión ardilla</div>
              <div className="chipmunk-dialog__song">{file.name || file.filename}</div>
            </div>
          </div>
          <button onClick={() => setFile(null)} className="btn-icon-sm" title="Cerrar">
            <X size={14} />
          </button>
        </div>

        <div className="chipmunk-presets" role="group" aria-label="Estilos">
          {CHIPMUNK_PRESETS.map((preset) => (
            <button
              key={preset.id}
              type="button"
              className={`chipmunk-preset ${activePreset?.id === preset.id ? 'chipmunk-preset--active' : ''}`}
              onClick={() => applyPreset(preset)}
            >
              {preset.label}
            </button>
          ))}
        </div>

        <Slider
          label="Tono"
          value={pitch}
          display={formatSemitones(pitch)}
          min={1}
          max={12}
          step={0.5}
          onChange={setPitch}
        />
        <Slider
          label="Velocidad"
          value={speed}
          display={`${speed.toFixed(2)}×`}
          min={0.5}
          max={2}
          step={0.05}
          onChange={setSpeed}
        />
        <Slider
          label="Brillo"
          value={brightness}
          display={brightness ? `+${brightness} dB` : 'Sin cambio'}
          min={0}
          max={12}
          step={1}
          onChange={setBrightness}
        />

        <p className="chipmunk-dialog__hint">
          Se guarda como una canción nueva en la carpeta «Ardillas»; el original no cambia.
        </p>

        <div className="modal-actions">
          <button type="button" className="btn-secondary" onClick={() => setFile(null)}>Cancelar</button>
          <button type="button" className="btn-accent" onClick={submit} disabled={submitting}>
            {submitting ? 'Iniciando…' : 'Crear versión'}
          </button>
        </div>
      </div>
    </div>
  );
}
