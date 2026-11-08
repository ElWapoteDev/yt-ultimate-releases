import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fingerprintCompletionSummary, fingerprintLocalComparisonSummary, fingerprintProfileSummary, fingerprintProgressLabel, fingerprintRecognitionSummary, fingerprintSummary, fingerprintVocalSummary, followFingerprintJob } from '../../src/lib/fingerprint.js';

test('waits through long jobs and verification instead of assuming completion after eight seconds', async () => {
  let requests = 0;
  const updates = [];
  const completed = { status: 'completed', output: { filename: 'copy.wav' } };
  const result = await followFingerprintJob(async () => {
    requests += 1;
    return requests > 12 ? completed : { status: requests > 8 ? 'verifying' : 'processing', progress: requests * 7 };
  }, (job) => updates.push(job), { wait: async () => {} });
  assert.equal(result, completed);
  assert.equal(updates.length, 12);
});

test('reports failure and cancellation as terminal results', async () => {
  for (const status of ['error', 'canceled']) {
    const result = await followFingerprintJob(async () => ({ status, error: 'Failed' }), () => assert.fail());
    assert.equal(result.status, status);
  }
});

test('recovers transient errors but bounds permanent connection loss', async () => {
  let requests = 0;
  const result = await followFingerprintJob(async () => {
    if (++requests < 3) throw new Error('offline');
    return { status: 'completed' };
  }, () => {}, { wait: async () => {} });
  assert.equal(result.status, 'completed');
  requests = 0;
  await assert.rejects(followFingerprintJob(async () => {
    requests += 1;
    throw new Error('offline');
  }, () => {}, { wait: async () => {} }), /puede seguir en curso/);
  assert.equal(requests, 5);
});

test('stops polling a missing job and cancels on unmount', async () => {
  await assert.rejects(followFingerprintJob(async () => {
    throw { response: { status: 404 } };
  }, () => {}), /ya no está disponible/);
  const controller = new AbortController();
  const result = followFingerprintJob(async () => ({ status: 'processing' }), () => {
    controller.abort();
  }, { signal: controller.signal });
  await assert.rejects(result, { name: 'AbortError' });
});

test('does not label unverified or unchanged audio as a changed acoustic fingerprint', () => {
  assert.match(fingerprintSummary({ status: 'changed' }), /Chromaprint distinta/);
  assert.match(fingerprintSummary({ status: 'unchanged' }), /sigue igual/);
  assert.match(fingerprintSummary({ status: 'unavailable' }), /falta Chromaprint/);
  assert.match(fingerprintSummary({ status: 'insufficient_audio' }), /demasiado corto/);
  assert.match(fingerprintSummary(), /sin verificar/);
});

test('a changed local fingerprint never announces failed external recognition', () => {
  for (const recognition_tested of [false, undefined]) {
    const verification = { status: 'changed', recognition_tested };
    assert.equal(fingerprintRecognitionSummary(verification), 'Sin comprobar.');
    const summary = fingerprintCompletionSummary(verification);
    assert.match(summary, /Variación creada/);
    assert.match(summary, /Chromaprint distinta/);
    assert.match(summary, /Reconocimiento externo sin comprobar/);
    assert.doesNotMatch(summary, /protegida|no identificable|Protegidos/);
  }
  // A boolean recording a test is not evidence of that test's result.
  assert.match(fingerprintRecognitionSummary({ recognition_tested: true }), /consulta el resultado/);
  assert.match(fingerprintCompletionSummary({ status: 'unchanged' }), /sigue igual/);
});

test('local bit agreement describes alignment without turning similarity into protection', () => {
  const aligned = { bit_similarity: 0.2, compared_frames: 80 };
  const best_offset = { bit_similarity: 0.975, compared_frames: 76, offset_frames: 4 };
  const local_comparison = { metric: 'chromaprint_bit_agreement_v1', aligned, best_offset };
  const summary = fingerprintLocalComparisonSummary({ local_comparison });
  assert.match(summary, /97,5 %/);
  assert.match(summary, /76 subhuellas Chromaprint/);
  assert.match(summary, /No mide si un detector reconocerá/);
  assert.doesNotMatch(summary, /protec|probabilidad/);
  assert.match(fingerprintLocalComparisonSummary({
    local_comparison: { metric: local_comparison.metric, aligned },
  }), /20,0 %/);
  for (const invalid of [
    undefined,
    { metric: 'unknown', aligned },
    { metric: local_comparison.metric, aligned: { bit_similarity: NaN, compared_frames: 30 } },
    { metric: local_comparison.metric, aligned: { bit_similarity: 1.01, compared_frames: 30 } },
    { metric: local_comparison.metric, aligned: { bit_similarity: -0.1, compared_frames: 30 } },
    { metric: local_comparison.metric, aligned: { bit_similarity: 0.5, compared_frames: 3 } },
  ]) {
    assert.equal(fingerprintLocalComparisonSummary({ local_comparison: invalid }), null);
  }
});

test('vocal preservation requires a valid final sample audit and keeps the separated-stem scope', () => {
  const vocal_integrity = {
    status: 'verified', scope: 'separated_stem', frames: 44100, channels: 2,
    max_abs_error: 2 ** -24, tolerance: 2 ** -23,
  };
  const profile = { intensity: 'quality', vocals_preserved: true, vocal_integrity };
  assert.match(fingerprintVocalSummary(profile), /Voz separada comprobada en el FLAC final/);
  assert.match(fingerprintVocalSummary(profile), /precisión de 24 bits/);
  assert.match(fingerprintVocalSummary(profile), /separación por IA puede dejar artefactos/);
  assert.match(fingerprintVocalSummary({ vocals_preserved: true }), /sin verificar/);
  for (const invalid of [
    { status: 'failed' }, { scope: 'original_recording' }, { frames: 0 }, { channels: 0 },
    { max_abs_error: 1e-3 }, { max_abs_error: NaN }, { tolerance: 0.1 },
  ]) {
    assert.match(fingerprintVocalSummary({
      ...profile, vocal_integrity: { ...vocal_integrity, ...invalid },
    }), /sin verificar/);
  }
});

test('keeps historical quality profiles described as instrumental-only processing', () => {
  for (const version of [undefined, 4, 5]) {
    const profile = { version, intensity: 'quality', pitch_semitones: 0.3, speed: 1, vocals_preserved: true };
    const summary = fingerprintProfileSummary(profile);
    assert.match(summary, /CALIDAD/);
    assert.match(summary, /solo en la instrumental/);
    assert.doesNotMatch(summary, /semitonos|Equilibrada/);
    assert.match(fingerprintVocalSummary(profile), /Voz separada sin efectos/);
  }
  assert.equal(fingerprintProfileSummary({ intensity: 'strong', pitch_semitones: 0.8, speed: 1 }),
    'Intensa · +0.8 semitonos · 1×');
  assert.equal(fingerprintProfileSummary({ intensity: 'balanced', pitch_semitones: 0.3, speed: 1 }),
    'Equilibrada · +0.3 semitonos · 1×');
});

test('quality profile 6 describes shared changes and never reuses an old vocal preservation claim', () => {
  const profile = {
    version: 6, intensity: 'quality', vocals_preserved: false,
    pitch_semitones: 0.28, speed: 1.016,
  };
  assert.equal(fingerprintProfileSummary(profile), 'CALIDAD · cambios suaves en voz e instrumental');
  const summary = fingerprintVocalSummary(profile);
  assert.match(summary, /juntas cambios suaves de tono, velocidad y volumen/);
  assert.match(summary, /calidad se valora al escuchar cada resultado/);
  assert.doesNotMatch(summary, /sin efectos|comprobada|intacta|idéntica|protegida|indetectable/);
  // A carried-over pre-resampling audit must not imply unchanged final vocals.
  assert.equal(fingerprintVocalSummary({
    ...profile,
    vocals_preserved: true,
    vocal_integrity: {
      status: 'verified', scope: 'separated_stem', frames: 44100, channels: 2,
      max_abs_error: 2 ** -24, tolerance: 2 ** -23,
    },
  }), summary);
});

test('shows separation, instrumental processing, recombination and shared adjustments during quality jobs', () => {
  assert.equal(fingerprintProgressLabel('separating', 'quality'), 'Separando voz e instrumental');
  assert.equal(fingerprintProgressLabel('processing', 'quality'), 'Modificando instrumental');
  assert.equal(fingerprintProgressLabel('recombining', 'quality'), 'Uniendo voz e instrumental');
  assert.equal(fingerprintProgressLabel('remapping', 'quality'), 'Ajustando voz e instrumental');
  assert.equal(fingerprintProgressLabel('verifying', 'quality'), 'Comparando huellas');
  assert.equal(fingerprintProgressLabel('processing', 'strong'), 'Modificando audio');
});
