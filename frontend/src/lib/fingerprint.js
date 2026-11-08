export function fingerprintSummary(verification) {
  switch (verification?.status) {
    case 'changed': return 'Huella Chromaprint distinta en el fragmento analizado (hasta 120 s).';
    case 'unchanged': return 'Audio modificado; la huella Chromaprint sigue igual en el fragmento analizado.';
    case 'insufficient_audio': return 'Audio modificado; el fragmento es demasiado corto para comparar huellas.';
    case 'unavailable': return 'Audio modificado; falta Chromaprint para verificar la huella acústica.';
    default: return 'Huella acústica sin verificar.';
  }
}

export function fingerprintProfileSummary(profile) {
  if (profile?.intensity === 'quality') {
    return profile.version >= 6
      ? 'CALIDAD · cambios suaves en voz e instrumental'
      : 'CALIDAD · cambios solo en la instrumental';
  }
  return `${profile?.intensity === 'strong' ? 'Intensa' : 'Equilibrada'} · +${profile?.pitch_semitones} semitonos · ${profile?.speed}×`;
}

export function fingerprintRecognitionSummary(verification) {
  return verification?.recognition_tested === true
    ? 'Prueba externa registrada; consulta el resultado del detector.'
    : 'Sin comprobar.';
}

export function fingerprintCompletionSummary(verification) {
  const external = verification?.recognition_tested === true
    ? fingerprintRecognitionSummary(verification)
    : 'Reconocimiento externo sin comprobar.';
  return `Variación creada. ${fingerprintSummary(verification)} ${external}`;
}

export function fingerprintLocalComparisonSummary(verification) {
  const comparison = verification?.local_comparison;
  if (comparison?.metric !== 'chromaprint_bit_agreement_v1') return null;
  const value = comparison.best_offset ?? comparison.aligned;
  if (!Number.isFinite(value?.bit_similarity)
      || value.bit_similarity < 0 || value.bit_similarity > 1
      || !Number.isInteger(value.compared_frames) || value.compared_frames < 16) return null;
  const percentage = (value.bit_similarity * 100).toLocaleString('es', {
    minimumFractionDigits: 1, maximumFractionDigits: 1,
  });
  return `${percentage} % · ${value.compared_frames} subhuellas Chromaprint, con alineación local. No mide si un detector reconocerá la canción.`;
}

export function fingerprintVocalSummary(profile) {
  if (profile?.intensity === 'quality' && profile.version >= 6) {
    return 'Voz e instrumental reciben juntas cambios suaves de tono, velocidad y volumen. La calidad se valora al escuchar cada resultado.';
  }
  const verification = profile?.vocal_integrity;
  const verified = verification?.status === 'verified'
    && verification.scope === 'separated_stem'
    && Number.isInteger(verification.frames) && verification.frames > 0
    && Number.isInteger(verification.channels) && verification.channels > 0
    && Number.isFinite(verification.tolerance)
    && verification.tolerance > 0 && verification.tolerance <= 2 ** -23
    && Number.isFinite(verification.max_abs_error)
    && verification.max_abs_error >= 0
    && verification.max_abs_error <= verification.tolerance;
  if (verified) {
    return 'Voz separada comprobada en el FLAC final, dentro de la precisión de 24 bits. La separación por IA puede dejar artefactos.';
  }
  if (profile?.vocals_preserved) {
    return 'Voz separada sin efectos; conservación de muestras en la mezcla final sin verificar.';
  }
  return 'Conservación de la voz sin verificar.';
}

export function fingerprintProgressLabel(phase, intensity) {
  switch (phase) {
    case 'separating': return 'Separando voz e instrumental';
    case 'recombining': return 'Uniendo voz e instrumental';
    case 'remapping': return 'Ajustando voz e instrumental';
    case 'verifying': return 'Comparando huellas';
    default: return intensity === 'quality' ? 'Modificando instrumental' : 'Modificando audio';
  }
}

function pause(milliseconds, signal) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) { reject(new DOMException('Aborted', 'AbortError')); return; }
    const abort = () => {
      clearTimeout(timer);
      reject(new DOMException('Aborted', 'AbortError'));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', abort);
      resolve();
    }, milliseconds);
    signal?.addEventListener('abort', abort, { once: true });
  });
}

// Poll until the backend actually finishes. A lost connection is bounded and
// does not imply cancellation of the server job.
export async function followFingerprintJob(getJob, onProgress, {
  signal, wait = pause, maxFailures = 5,
} = {}) {
  let failures = 0;
  for (;;) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    let job;
    try {
      job = await getJob();
      failures = 0;
    } catch (error) {
      if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
      if (error?.response?.status === 404) throw new Error('El trabajo ya no está disponible. Revisa Canciones.');
      failures += 1;
      if (failures >= maxFailures) {
        throw new Error('Se perdió la conexión. El trabajo puede seguir en curso; revisa Canciones al reconectar.');
      }
      await wait(1500, signal);
      continue;
    }
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    if (['completed', 'error', 'canceled'].includes(job.status)) return job;
    onProgress(job);
    await wait(1500, signal);
  }
}
