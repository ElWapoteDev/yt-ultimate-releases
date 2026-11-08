import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { constants as fsConstants } from 'node:fs';
import {
  access,
  mkdtemp,
  mkdir,
  readdir,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  findPackagedApp,
  pathExists,
} from './lib/packaged-app.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const productName = process.env.YTU_PRODUCT_NAME || 'YT Ultimate';
const maximumCaptureBytes = 64 * 1024 * 1024;
const defaultTimeoutMs = 30 * 60 * 1_000;
const syntheticDurationSeconds = 12;
const expectedOutputNames = {
  Vocals: 'ytu-processing-smoke-vocals',
  Instrumental: 'ytu-processing-smoke-instrumental',
};

function report(message) {
  process.stdout.write(`[processing-smoke] ✓ ${message}\n`);
}

function parseTimeout(value) {
  if (!value) return defaultTimeoutMs;
  const timeoutMs = Number(value);
  assert.ok(
    Number.isSafeInteger(timeoutMs) && timeoutMs >= 60_000,
    'YTU_PROCESSING_SMOKE_TIMEOUT_MS debe ser un entero de al menos 60000.',
  );
  return timeoutMs;
}

function signalProcessGroup(child, signal) {
  if (!child.pid) return;
  try {
    process.kill(-child.pid, signal);
  } catch (error) {
    if (error.code === 'ESRCH') return;
    try {
      child.kill(signal);
    } catch (fallbackError) {
      if (fallbackError.code !== 'ESRCH') throw fallbackError;
    }
  }
}

function runCapturedCommand(executable, args, {
  cwd,
  env = process.env,
  description,
  timeoutMs,
}) {
  return new Promise((resolve) => {
    const startedAt = Date.now();
    const stdoutChunks = [];
    const stderrChunks = [];
    let capturedBytes = 0;
    let outputOverflow = false;
    let timedOut = false;
    let stopping = false;
    let spawnError = null;
    let forceKillTimer = null;

    const child = spawn(executable, args, {
      cwd,
      env,
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    const stop = () => {
      if (stopping) return;
      stopping = true;
      try {
        signalProcessGroup(child, 'SIGTERM');
      } catch {
        child.kill('SIGTERM');
      }
      forceKillTimer = setTimeout(() => {
        try {
          signalProcessGroup(child, 'SIGKILL');
        } catch {
          child.kill('SIGKILL');
        }
      }, 5_000);
      forceKillTimer.unref();
    };

    const capture = (chunks, chunk) => {
      capturedBytes += chunk.length;
      if (capturedBytes > maximumCaptureBytes) {
        if (!outputOverflow) {
          outputOverflow = true;
          stop();
        }
        return;
      }
      chunks.push(Buffer.from(chunk));
    };

    child.stdout.on('data', (chunk) => capture(stdoutChunks, chunk));
    child.stderr.on('data', (chunk) => capture(stderrChunks, chunk));
    child.once('error', (error) => {
      spawnError = error;
    });

    const timeout = setTimeout(() => {
      timedOut = true;
      stop();
    }, timeoutMs);
    timeout.unref();

    const heartbeat = setInterval(() => {
      const elapsedSeconds = Math.round((Date.now() - startedAt) / 1_000);
      process.stdout.write(
        `[processing-smoke] … ${description} (${elapsedSeconds} s)\n`,
      );
    }, 30_000);
    heartbeat.unref();

    child.once('close', (code, signal) => {
      clearTimeout(timeout);
      clearTimeout(forceKillTimer);
      clearInterval(heartbeat);
      resolve({
        code,
        signal,
        spawnError,
        timedOut,
        outputOverflow,
        stdout: Buffer.concat(stdoutChunks).toString('utf8'),
        stderr: Buffer.concat(stderrChunks).toString('utf8'),
        durationMs: Date.now() - startedAt,
      });
    });
  });
}

function errorLines(stdout, stderr) {
  const patterns = [
    /\bTraceback \(most recent call last\):/i,
    /\b[A-Za-z][A-Za-z0-9_]*(?:Error|Exception):/,
    /\[(?:ERROR|CRITICAL|FATAL)\]/i,
    /\b(?:ERROR|CRITICAL|FATAL)\s+-/i,
    /^\s*(?:ERROR|CRITICAL|FATAL)(?::|\s+-)/i,
    /\b(?:segmentation fault|abort trap|illegal instruction|bus error)\b/i,
    /\bkilled:\s*9\b/i,
    /\bfailed to\b/i,
  ];
  const ansiEscape = new RegExp(
    `${String.fromCharCode(27)}\\[[0-?]*[ -/]*[@-~]`,
    'g',
  );
  return `${stdout}\n${stderr}`
    .replace(ansiEscape, '')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && patterns.some((pattern) => pattern.test(line)));
}

function outputTail(stdout, stderr, maximumCharacters = 12_000) {
  const output = [
    '--- stdout ---',
    stdout.trim() || '(vacío)',
    '--- stderr ---',
    stderr.trim() || '(vacío)',
  ].join('\n');
  return output.slice(-maximumCharacters);
}

async function executeChecked({
  executable,
  args,
  cwd,
  env,
  description,
  logName,
  logsDir,
  timeoutMs,
}) {
  const result = await runCapturedCommand(executable, args, {
    cwd,
    env,
    description,
    timeoutMs,
  });
  await Promise.all([
    writeFile(
      path.join(logsDir, `${logName}.stdout.log`),
      result.stdout,
      'utf8',
    ),
    writeFile(
      path.join(logsDir, `${logName}.stderr.log`),
      result.stderr,
      'utf8',
    ),
  ]);

  const failures = [];
  if (result.spawnError) failures.push(result.spawnError.message);
  if (result.timedOut) failures.push(`excedió ${timeoutMs} ms`);
  if (result.outputOverflow) {
    failures.push(`generó más de ${maximumCaptureBytes} bytes de salida`);
  }
  if (result.code !== 0) {
    failures.push(
      `terminó con ${result.code ?? result.signal ?? 'estado desconocido'}`,
    );
  }
  const reportedErrors = errorLines(result.stdout, result.stderr);
  if (reportedErrors.length) {
    failures.push(
      `reportó errores aunque su código de salida fuera ${result.code}:`
      + `\n${reportedErrors.slice(0, 20).join('\n')}`,
    );
  }
  if (failures.length) {
    throw new Error(
      `${description} falló: ${failures.join('; ')}\n`
      + outputTail(result.stdout, result.stderr),
    );
  }

  const durationSeconds = (result.durationMs / 1_000).toFixed(1);
  report(`${description} (${durationSeconds} s)`);
  return result;
}

async function requireFile(filePath, {
  executable = false,
  minimumBytes = 1,
} = {}) {
  const details = await stat(filePath);
  assert.ok(details.isFile(), `No es un archivo regular: ${filePath}`);
  assert.ok(
    details.size >= minimumBytes,
    `Archivo incompleto (${details.size} bytes): ${filePath}`,
  );
  if (executable) await access(filePath, fsConstants.X_OK);
  return details;
}

async function selectModel(modelsDir) {
  const configuredModel = (
    process.env.YTU_PROCESSING_SMOKE_MODEL || ''
  ).trim();
  if (configuredModel) {
    assert.equal(
      path.basename(configuredModel),
      configuredModel,
      'YTU_PROCESSING_SMOKE_MODEL debe contener sólo el nombre del checkpoint.',
    );
    assert.match(
      configuredModel,
      /bs[_-]?roformer.*\.ckpt$/i,
      'YTU_PROCESSING_SMOKE_MODEL debe ser un checkpoint BS-RoFormer.',
    );
    return configuredModel;
  }

  const entries = await readdir(modelsDir, { withFileTypes: true });
  const candidates = entries
    .filter(
      (entry) => entry.isFile()
        && /bs[_-]?roformer.*\.ckpt$/i.test(entry.name),
    )
    .map((entry) => entry.name)
    .sort((left, right) => left.localeCompare(right));
  const pairedCandidates = [];
  for (const checkpoint of candidates) {
    const configuration = path.join(
      modelsDir,
      `${path.basename(checkpoint, '.ckpt')}.yaml`,
    );
    if (await pathExists(configuration)) pairedCandidates.push(checkpoint);
  }
  assert.ok(
    pairedCandidates.length,
    `No hay un checkpoint BS-RoFormer con YAML dentro de ${modelsDir}.`,
  );
  return pairedCandidates[0];
}

async function probeWave({
  ffprobe,
  wavePath,
  cwd,
  logsDir,
  logName,
  timeoutMs,
}) {
  const result = await executeChecked({
    executable: ffprobe,
    args: [
      '-v',
      'error',
      '-show_entries',
      'format=duration,size:stream=codec_type,codec_name',
      '-of',
      'json',
      wavePath,
    ],
    cwd,
    description: `validación de ${path.basename(wavePath)}`,
    logName,
    logsDir,
    timeoutMs: Math.min(timeoutMs, 60_000),
  });
  let metadata;
  try {
    metadata = JSON.parse(result.stdout);
  } catch (error) {
    throw new Error(
      `FFprobe no devolvió JSON válido para ${wavePath}: ${error.message}`,
      { cause: error },
    );
  }
  const audioStreams = (metadata.streams || []).filter(
    (stream) => stream.codec_type === 'audio',
  );
  assert.ok(audioStreams.length, `El stem no contiene audio: ${wavePath}`);
  assert.ok(
    audioStreams.every((stream) => /^pcm_/i.test(stream.codec_name || '')),
    `El stem no contiene PCM WAV válido: ${wavePath}`,
  );
  const duration = Number(metadata.format?.duration);
  assert.ok(
    Number.isFinite(duration) && duration > 0,
    `El stem tiene duración inválida: ${wavePath}`,
  );
  return duration;
}

async function main() {
  assert.equal(process.platform, 'darwin', 'Este smoke test requiere macOS.');
  const timeoutMs = parseTimeout(
    process.env.YTU_PROCESSING_SMOKE_TIMEOUT_MS,
  );
  const appPath = await findPackagedApp({ frontendDir, productName });
  const resourcesDir = path.join(appPath, 'Contents', 'Resources');
  const binDir = path.join(resourcesDir, 'bin');
  const modelsDir = path.join(resourcesDir, 'models');
  const paths = {
    ffmpeg: path.join(binDir, 'ffmpeg'),
    ffprobe: path.join(binDir, 'ffprobe'),
    separator: path.join(resourcesDir, 'backend', 'audio-separator'),
  };
  const modelName = await selectModel(modelsDir);
  const checkpoint = path.join(modelsDir, modelName);
  const configuration = path.join(
    modelsDir,
    `${path.basename(modelName, '.ckpt')}.yaml`,
  );
  await Promise.all([
    requireFile(paths.ffmpeg, { executable: true, minimumBytes: 1_000_000 }),
    requireFile(paths.ffprobe, { executable: true, minimumBytes: 1_000_000 }),
    requireFile(paths.separator, { executable: true, minimumBytes: 100_000 }),
    requireFile(checkpoint, { minimumBytes: 1_000_000 }),
    requireFile(configuration, { minimumBytes: 100 }),
  ]);
  report(`bundle seleccionado: ${appPath}`);
  report(`modelo seleccionado: ${modelName}`);

  const temporaryRoot = await mkdtemp(
    path.join(os.tmpdir(), 'ytu-processing-smoke-'),
  );
  const outputDir = path.join(temporaryRoot, 'outputs');
  const logsDir = path.join(temporaryRoot, 'logs');
  const numbaCacheDir = path.join(temporaryRoot, 'numba-cache');
  const cacheDir = path.join(temporaryRoot, 'cache');
  const torchCacheDir = path.join(temporaryRoot, 'torch-cache');
  const matplotlibConfigDir = path.join(temporaryRoot, 'matplotlib');
  const runtimeTempDir = path.join(temporaryRoot, 'tmp');
  const inputWave = path.join(temporaryRoot, 'synthetic-input.wav');
  let passed = false;

  try {
    await Promise.all([
      mkdir(outputDir),
      mkdir(logsDir),
      mkdir(numbaCacheDir),
      mkdir(cacheDir),
      mkdir(torchCacheDir),
      mkdir(matplotlibConfigDir),
      mkdir(runtimeTempDir),
    ]);
    await access(numbaCacheDir, fsConstants.W_OK);

    await executeChecked({
      executable: paths.ffmpeg,
      args: [
        '-hide_banner',
        '-nostdin',
        '-loglevel',
        'error',
        '-y',
        '-f',
        'lavfi',
        '-i',
        'aevalsrc=0.20*sin(2*PI*220*t)+0.12*sin(2*PI*660*t)'
          + `:s=44100:d=${syntheticDurationSeconds}`,
        '-f',
        'lavfi',
        '-i',
        'anoisesrc=color=pink:amplitude=0.08:sample_rate=44100'
          + `:duration=${syntheticDurationSeconds}:seed=20260729`,
        '-filter_complex',
        '[0:a][1:a]amix=inputs=2:duration=shortest:normalize=0,'
          + 'pan=stereo|c0=c0|c1=c0[synthetic]',
        '-map',
        '[synthetic]',
        '-c:a',
        'pcm_s16le',
        inputWave,
      ],
      cwd: temporaryRoot,
      description: 'generación del WAV sintético con FFmpeg empaquetado',
      logName: '01-ffmpeg-generate',
      logsDir,
      timeoutMs: Math.min(timeoutMs, 60_000),
    });
    await requireFile(inputWave, { minimumBytes: 1_000_000 });

    const separatorEnvironment = {
      ...process.env,
      AUDIO_SEPARATOR_MODEL_DIR: modelsDir,
      FFMPEG_PATH: paths.ffmpeg,
      FFPROBE_PATH: paths.ffprobe,
      HF_HOME: path.join(cacheDir, 'huggingface'),
      MPLCONFIGDIR: matplotlibConfigDir,
      NUMBA_CACHE_DIR: numbaCacheDir,
      PATH: [binDir, process.env.PATH].filter(Boolean).join(path.delimiter),
      PYTHONUNBUFFERED: '1',
      TMPDIR: runtimeTempDir,
      TORCH_HOME: torchCacheDir,
      XDG_CACHE_HOME: cacheDir,
    };
    await executeChecked({
      executable: paths.separator,
      args: [
        '--log_level',
        'info',
        '--model_filename',
        modelName,
        '--model_file_dir',
        modelsDir,
        '--output_dir',
        outputDir,
        '--output_format',
        'WAV',
        '--sample_rate',
        '44100',
        '--custom_output_names',
        JSON.stringify(expectedOutputNames),
        inputWave,
      ],
      cwd: resourcesDir,
      env: separatorEnvironment,
      description: 'separación real con audio-separator empaquetado',
      logName: '02-audio-separator',
      logsDir,
      timeoutMs,
    });

    const outputEntries = await readdir(outputDir, { withFileTypes: true });
    const waveNames = outputEntries
      .filter(
        (entry) => entry.isFile() && path.extname(entry.name).toLowerCase()
          === '.wav',
      )
      .map((entry) => entry.name)
      .sort((left, right) => left.localeCompare(right));
    assert.equal(
      waveNames.length,
      2,
      `Se esperaban exactamente 2 stems WAV y se encontraron `
      + `${waveNames.length}: ${waveNames.join(', ') || '(ninguno)'}`,
    );

    const stemPaths = [];
    for (const [stem, expectedName] of Object.entries(expectedOutputNames)) {
      const matches = waveNames.filter((name) => (
        name.toLowerCase().includes(expectedName.toLowerCase())
      ));
      assert.equal(
        matches.length,
        1,
        `No se encontró exactamente un stem ${stem}: ${waveNames.join(', ')}`,
      );
      const stemPath = path.join(outputDir, matches[0]);
      await requireFile(stemPath, { minimumBytes: 1_000_000 });
      stemPaths.push({ stem, path: stemPath });
    }

    for (const [index, stem] of stemPaths.entries()) {
      const duration = await probeWave({
        ffprobe: paths.ffprobe,
        wavePath: stem.path,
        cwd: temporaryRoot,
        logsDir,
        logName: `0${index + 3}-ffprobe-${stem.stem.toLowerCase()}`,
        timeoutMs,
      });
      assert.ok(
        Math.abs(duration - syntheticDurationSeconds) <= 0.25,
        `${stem.stem} dura ${duration} s; se esperaban `
        + `${syntheticDurationSeconds} s.`,
      );
      report(
        `${stem.stem}: ${path.basename(stem.path)} `
        + `(${duration.toFixed(3)} s)`,
      );
    }

    passed = true;
    report('procesamiento real completado con dos stems WAV válidos');
  } finally {
    const keepData = process.env.YTU_KEEP_PROCESSING_SMOKE_DATA === '1';
    if (passed && !keepData) {
      await rm(temporaryRoot, { recursive: true, force: true });
    } else {
      const reason = passed ? 'YTU_KEEP_PROCESSING_SMOKE_DATA=1' : 'fallo';
      process.stderr.write(
        `[processing-smoke] datos conservados por ${reason}: `
        + `${temporaryRoot}\n`,
      );
    }
  }
}

main().catch((error) => {
  let details = error.stack || String(error);
  if (error.cause) {
    details += `\nCausa: ${error.cause.stack || error.cause}`;
  }
  process.stderr.write(`[processing-smoke] ✗ ${details}\n`);
  process.exitCode = 1;
});
