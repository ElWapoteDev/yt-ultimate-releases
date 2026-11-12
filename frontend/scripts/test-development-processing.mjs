import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { access, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BackendController } from '../electron/backend-controller.mjs';

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const projectDir = path.resolve(frontendDir, '..');
const backendDir = path.join(projectDir, 'backend');
const captureLimit = 64 * 1024;

function stopProcessTree(child) {
  if (!child.pid) return;
  if (process.platform === 'win32') {
    execFile('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true }, () => {});
  } else {
    try { process.kill(-child.pid, 'SIGKILL'); } catch (error) {
      if (error.code !== 'ESRCH') throw error;
    }
  }
}

async function main() {
  const timeoutMs = Number(process.env.YTU_DEV_PROCESSING_TIMEOUT_MS || 600_000);
  assert.ok(Number.isSafeInteger(timeoutMs) && timeoutMs >= 60_000,
    'YTU_DEV_PROCESSING_TIMEOUT_MS debe ser un entero de al menos 60000.');
  // Electron provides this property. Plain Node needs a host placeholder even
  // though isDev selects the project's resources in BackendController.
  if (!process.resourcesPath) process.resourcesPath = frontendDir;
  const controller = new BackendController({ app: {}, projectDir, isDev: true });
  const { python } = controller.developmentRuntime();
  const binaryEnvironment = controller.binaryEnvironmentVariables();
  for (const variable of ['YTU_FFMPEG_PATH', 'YTU_FFPROBE_PATH', 'YTU_FPCALC_PATH']) {
    assert.ok(binaryEnvironment[variable], `Falta ${variable}; ejecuta el staging de herramientas nativas.`);
    await access(binaryEnvironment[variable]);
  }
  const modelDir = path.join(backendDir, 'models', 'audio-separator');
  await access(path.join(modelDir, 'model_bs_roformer_ep_317_sdr_12.9755.ckpt'));
  await access(path.join(modelDir, 'model_bs_roformer_ep_317_sdr_12.9755.yaml'));
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'ytu-dev-processing-'));
  const environment = Object.fromEntries(
    Object.entries(process.env).filter(([name]) => (
      !name.toUpperCase().startsWith('YTU_') && name.toUpperCase() !== 'PYTHONOPTIMIZE'
    )),
  );
  let captured = '';
  let failure;
  try {
    await new Promise((resolve, reject) => {
      // Do not optimize this parent process or replace separator arguments in
      // the test: the desktop's actual environment must survive AppConfig.
      const child = spawn(python, [
        '-u', path.join(backendDir, 'tests', 'smoke_quality_processing.py'), temporary,
      ], {
        cwd: backendDir,
        detached: process.platform !== 'win32',
        windowsHide: true,
        env: {
          ...environment,
          ...binaryEnvironment,
          PYTHONUNBUFFERED: '1',
          PYTHON_COLORS: '0',
          NO_COLOR: '1',
          YTU_RESOURCE_DIR: backendDir,
          YTU_DATA_DIR: path.join(temporary, 'data'),
          YTU_CACHE_DIR: path.join(temporary, 'cache'),
          YTU_LOG_DIR: path.join(temporary, 'logs'),
          YTU_LIBRARY_DIR: path.join(temporary, 'library'),
          YTU_MODEL_DIR: modelDir,
          YTU_PLUGIN_DIR: path.join(temporary, 'plugins'),
          YTU_DATABASE_PATH: path.join(temporary, 'data', 'smoke.sqlite3'),
          YTU_DEV_PROCESSING_TIMEOUT_MS: String(timeoutMs),
          NUMBA_CACHE_DIR: path.join(temporary, 'numba'),
          MPLCONFIGDIR: path.join(temporary, 'matplotlib'),
          XDG_CACHE_HOME: path.join(temporary, 'xdg'),
          TORCH_HOME: path.join(temporary, 'torch'),
          HF_HOME: path.join(temporary, 'huggingface'),
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      let timedOut = false;
      const timeout = setTimeout(() => {
        timedOut = true;
        stopProcessTree(child);
      }, timeoutMs + 30_000);
      const capture = (chunk) => {
        const text = chunk.toString('utf8');
        captured = (captured + text).slice(-captureLimit);
        process.stdout.write(text);
      };
      child.stdout.on('data', capture);
      child.stderr.on('data', capture);
      child.once('error', (error) => { clearTimeout(timeout); reject(error); });
      child.once('close', (code, signal) => {
        clearTimeout(timeout);
        if (code === 0 && !timedOut) resolve();
        else reject(new Error(timedOut
          ? 'La prueba de procesamiento excedió el tiempo límite.'
          : `La prueba de procesamiento terminó con ${code ?? signal}.`));
      });
    });
  } catch (error) {
    failure = error;
  } finally {
    await rm(temporary, { recursive: true, force: true });
    if (failure) {
      // Keep only diagnostics; no generated audio or cache remains after a
      // failed smoke test, and the user's library was never opened.
      await mkdir(temporary);
      const log = path.join(temporary, 'failure.log');
      await writeFile(log, `${captured}\n${failure.stack || failure}\n`);
      process.stderr.write(`[dev-processing] diagnóstico: ${log}\n`);
    }
  }
  if (failure) throw failure;
  process.stdout.write('[dev-processing] ✓ CALIDAD completado mediante la configuración real de desarrollo\n');
}

main().catch((error) => {
  process.stderr.write(`[dev-processing] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
