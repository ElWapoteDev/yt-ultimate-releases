import { execFile, spawn } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import { createWriteStream, existsSync } from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { isWindows } from './platform.mjs';

const LOOPBACK_HOST = '127.0.0.1';
const STARTUP_TIMEOUT_MS = 90_000;
// Windows shuts down through stdin and gives open streams two seconds to end.
const STOP_GRACE_MS = isWindows ? 5_000 : 3_000;
// The packaged Windows service runs on an embedded CPython whose ._pth file
// defines sys.path. -I keeps user site-packages and PYTHON* variables from
// leaking in, -O matches the optimize=1 PyInstaller sidecar on macOS (which
// also turns beartype off for the separator's model classes), and -X utf8
// decodes FFmpeg and separator output as UTF-8.
const WINDOWS_RUNTIME_FLAGS = Object.freeze(['-I', '-O', '-X', 'utf8', '-u']);
const WINDOWS_DEVELOPMENT_FLAGS = Object.freeze(['-X', 'utf8', '-u']);

async function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', reject);
    server.listen(0, LOOPBACK_HOST, () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      server.close((error) => {
        if (error) reject(error);
        else resolve(port);
      });
    });
  });
}

function firstExisting(candidates) {
  return candidates.find((candidate) => existsSync(candidate)) || '';
}

function killProcessTree(pid) {
  const taskkill = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'taskkill.exe');
  return new Promise((resolve) => {
    execFile(taskkill, ['/PID', String(pid), '/T', '/F'], { windowsHide: true }, () => resolve());
  });
}

export class BackendController {
  constructor({ app, projectDir, isDev, cacheDir = '', onState = () => {} }) {
    this.app = app;
    this.projectDir = projectDir;
    this.isDev = isDev;
    this.cacheDir = cacheDir;
    this.process = null;
    this.logStream = null;
    this.runtime = null;
    this.stopping = false;
    this.spawnError = null;
    this.onState = onState;
  }

  windowsRuntime() {
    const backendRoot = path.join(process.resourcesPath, 'backend');
    const python = path.join(backendRoot, 'python', 'python.exe');
    const launcher = path.join(backendRoot, 'app', 'launcher.py');
    if (!existsSync(python) || !existsSync(launcher)) {
      throw new Error('El servicio incluido en la aplicación no está disponible.');
    }
    return { python, launcher };
  }

  developmentRuntime(platform = process.platform) {
    const backendDir = path.resolve(this.projectDir, 'backend');
    const windows = platform === 'win32';
    const python = firstExisting(windows
      ? [path.join(backendDir, 'venv', 'Scripts', 'python.exe')]
      : [path.join(backendDir, 'venv', 'bin', 'python3'), path.join(backendDir, 'venv', 'bin', 'python')]);
    if (!python) {
      throw new Error(windows
        ? 'No se encontró backend\\venv\\Scripts\\python.exe para desarrollo.'
        : 'No se encontró backend/venv/bin/python3 para desarrollo.');
    }
    const launcher = path.join(backendDir, 'launcher.py');
    if (!existsSync(launcher)) {
      throw new Error('No se encontró backend/launcher.py para iniciar el separador.');
    }
    return { python, launcher };
  }

  developmentSeparator(platform = process.platform) {
    const { python, launcher } = this.developmentRuntime(platform);
    // Match the packaged runtime: the model's beartype decorators do not
    // support the annotations used by RoFormer on newer Python versions.
    // Calling the console script directly loses -O, even if the parent has it.
    return {
      separator: python,
      separatorArgs: ['-O', '-X', 'utf8', '-u', launcher, '--audio-separator'],
    };
  }

  command() {
    if (!this.isDev) {
      if (isWindows) {
        const { python, launcher } = this.windowsRuntime();
        return { executable: python, args: [...WINDOWS_RUNTIME_FLAGS, launcher] };
      }
      const executable = firstExisting([
        path.join(process.resourcesPath, 'backend', 'yt-ultimate-backend'),
        path.join(process.resourcesPath, 'backend', 'yt-ultimate-backend.bin'),
      ]);
      if (!executable) {
        throw new Error('El servicio incluido en la aplicación no está disponible.');
      }
      return { executable, args: [] };
    }

    const { python: executable } = this.developmentRuntime();
    if (isWindows) {
      return { executable, args: [...WINDOWS_DEVELOPMENT_FLAGS, '-m', 'ytultimate'] };
    }
    return { executable, args: ['-m', 'ytultimate'] };
  }

  windowsBinaryEnvironment() {
    const nativeBin = this.isDev
      ? path.join(this.projectDir, 'frontend', '.electron-bin-win')
      : path.join(process.resourcesPath, 'bin');
    const ffmpeg = firstExisting([path.join(nativeBin, 'ffmpeg.exe')]);
    const ffprobe = firstExisting([path.join(nativeBin, 'ffprobe.exe')]);
    const fpcalc = firstExisting([path.join(nativeBin, 'fpcalc.exe')]);
    if (this.isDev) {
      return { ffmpeg, ffprobe, fpcalc, ...this.developmentSeparator('win32') };
    }
    // The separator is the same embedded interpreter and entry point started
    // in separator mode, so no second frozen executable is shipped.
    const { python, launcher } = this.windowsRuntime();
    return {
      ffmpeg,
      ffprobe,
      fpcalc,
      separator: python,
      separatorArgs: [...WINDOWS_RUNTIME_FLAGS, launcher, '--audio-separator'],
    };
  }

  binaryEnvironment() {
    if (isWindows) return this.windowsBinaryEnvironment();
    const bundledBin = path.join(process.resourcesPath, 'bin');
    const developmentBin = path.join(this.projectDir, 'frontend', '.electron-bin');
    const nativeBin = this.isDev ? developmentBin : bundledBin;
    const fpcalc = firstExisting([
      path.join(nativeBin, 'fpcalc'),
      '/opt/homebrew/bin/fpcalc',
      '/usr/local/bin/fpcalc',
    ]);
    const bundledBackend = path.join(process.resourcesPath, 'backend');
    const ffmpeg = firstExisting([
      path.join(nativeBin, 'ffmpeg'),
      '/opt/homebrew/bin/ffmpeg',
      '/usr/local/bin/ffmpeg',
      '/usr/bin/ffmpeg',
    ]);
    const ffprobe = firstExisting([
      path.join(nativeBin, 'ffprobe'),
      '/opt/homebrew/bin/ffprobe',
      '/usr/local/bin/ffprobe',
      '/usr/bin/ffprobe',
    ]);
    if (this.isDev) {
      return { ffmpeg, ffprobe, fpcalc, ...this.developmentSeparator() };
    }
    const separator = firstExisting([
      path.join(bundledBackend, 'audio-separator'),
      '/opt/homebrew/bin/audio-separator',
      '/usr/local/bin/audio-separator',
    ]);
    return { ffmpeg, ffprobe, fpcalc, separator, separatorArgs: [] };
  }

  binaryEnvironmentVariables() {
    const { ffmpeg, ffprobe, fpcalc, separator, separatorArgs } = this.binaryEnvironment();
    return {
      ...(ffmpeg ? { YTU_FFMPEG_PATH: ffmpeg } : {}),
      ...(ffprobe ? { YTU_FFPROBE_PATH: ffprobe } : {}),
      ...(fpcalc ? { YTU_FPCALC_PATH: fpcalc } : {}),
      ...(separator ? {
        YTU_SEPARATOR_PATH: separator,
        // The executable and its arguments must be replaced together. An
        // inherited prefix for a different interpreter breaks a frozen sidecar.
        YTU_SEPARATOR_ARGS: JSON.stringify(separatorArgs),
      } : {}),
    };
  }

  async start() {
    if (this.process && this.runtime) return this.runtime;

    this.stopping = false;
    this.spawnError = null;
    this.onState({ status: 'starting' });
    const existingRuntime = this.runtime;
    const port = existingRuntime
      ? Number(new URL(existingRuntime.apiOrigin).port)
      : await reservePort();
    const authToken = existingRuntime?.authToken
      || randomBytes(32).toString('base64url');
    const instanceId = existingRuntime?.instanceId || randomUUID();
    const apiOrigin = `http://${LOOPBACK_HOST}:${port}`;
    const apiBaseUrl = `${apiOrigin}/api/v1`;
    const backendDir = path.join(this.projectDir, 'backend');
    const { executable, args } = this.command();
    const binaryVariables = this.binaryEnvironmentVariables();

    const dataDir = this.app.getPath('userData');
    const logDir = this.app.getPath('logs');
    const cacheDir = this.cacheDir || this.app.getPath('cache');
    const resourceDir = this.isDev ? backendDir : process.resourcesPath;
    const modelDir = this.isDev
      ? path.join(backendDir, 'models', 'audio-separator')
      : path.join(dataDir, 'Models');
    const pluginDir = path.join(dataDir, 'Plugins');
    const logPath = path.join(logDir, 'backend.log');

    this.logStream = createWriteStream(logPath, { flags: 'a' });
    this.runtime = {
      ...(existingRuntime || {}),
      apiOrigin,
      apiBaseUrl,
      authToken,
      instanceId,
      isDesktop: true,
      platform: process.platform,
      appVersion: this.app.getVersion(),
      paths: {
        data: dataDir,
        logs: logDir,
        plugins: pluginDir,
        models: modelDir,
      },
    };

    const inheritedEnvironment = { ...process.env };
    if (isWindows && !this.isDev) {
      // A system-wide Python installation must not redirect the embedded
      // runtime; Windows variable names are case-insensitive.
      for (const name of Object.keys(inheritedEnvironment)) {
        if (['PYTHONHOME', 'PYTHONPATH'].includes(name.toUpperCase())) {
          delete inheritedEnvironment[name];
        }
      }
    }
    const child = spawn(executable, args, {
      cwd: backendDir,
      detached: !isWindows,
      windowsHide: true,
      env: {
        ...inheritedEnvironment,
        PYTHONUNBUFFERED: '1',
        YTU_HOST: LOOPBACK_HOST,
        YTU_PORT: String(port),
        YTU_AUTH_TOKEN: authToken,
        YTU_INSTANCE_ID: instanceId,
        YTU_DATA_DIR: dataDir,
        YTU_CACHE_DIR: cacheDir,
        YTU_LOG_DIR: logDir,
        NUMBA_CACHE_DIR: path.join(cacheDir, 'Numba'),
        YTU_RESOURCE_DIR: resourceDir,
        YTU_MODEL_DIR: modelDir,
        YTU_PLUGIN_DIR: pluginDir,
        ...binaryVariables,
        // Windows cannot deliver SIGTERM, so closing stdin requests shutdown.
        ...(isWindows ? { PYTHONUTF8: '1', YTU_EXIT_ON_STDIN_CLOSE: '1' } : {}),
      },
      stdio: [isWindows ? 'pipe' : 'ignore', 'pipe', 'pipe'],
    });
    this.process = child;
    child.stdin?.on('error', () => {});

    const forward = (label, stream, consoleMethod) => {
      stream?.on('data', (chunk) => {
        const line = `[${new Date().toISOString()}] [${label}] ${chunk}`;
        this.logStream?.write(line);
        if (this.isDev) consoleMethod(String(chunk).trimEnd());
      });
    };
    forward('stdout', child.stdout, console.log);
    forward('stderr', child.stderr, console.error);

    child.once('error', (error) => {
      this.spawnError = error;
      this.onState({
        status: 'error',
        message: error.message,
      });
    });

    child.once('exit', (code, signal) => {
      const expected = this.stopping;
      if (this.process === child) this.process = null;
      this.logStream?.end();
      this.logStream = null;
      if (!expected) {
        const message = `El backend terminó inesperadamente (${code ?? signal ?? 'desconocido'}).`;
        console.error(message);
        this.onState({ status: 'error', message, code, signal });
      }
    });

    await this.waitUntilReady();
    this.onState({ status: 'ready', runtime: this.runtime });
    return this.runtime;
  }

  async waitUntilReady() {
    const startedAt = Date.now();
    let lastError = null;
    while (Date.now() - startedAt < STARTUP_TIMEOUT_MS) {
      if (this.spawnError) throw this.spawnError;
      if (!this.process) {
        throw new Error('El servicio local terminó durante el arranque.');
      }
      try {
        const response = await fetch(`${this.runtime.apiBaseUrl}/system/health`, {
          headers: { 'X-YTU-Token': this.runtime.authToken },
          signal: AbortSignal.timeout(1_500),
        });
        if (response.ok) {
          const health = await response.json();
          if (!health.instance_id || health.instance_id === this.runtime.instanceId) {
            const reportedPaths = health.paths || {};
            this.runtime.paths = {
              ...this.runtime.paths,
              ...(health.library_path ? { library: health.library_path } : {}),
              ...reportedPaths,
            };
            return;
          }
          throw new Error('Respondió otra instancia del servicio local.');
        }
        lastError = new Error(`El servicio respondió HTTP ${response.status}.`);
      } catch (error) {
        lastError = error;
      }
      await new Promise((resolve) => setTimeout(resolve, 300));
    }
    throw new Error(`El servicio local no inició a tiempo. ${lastError?.message || ''}`.trim());
  }

  async request(pathname, options = {}) {
    if (!this.runtime) throw new Error('El backend todavía no está disponible.');
    const response = await fetch(`${this.runtime.apiBaseUrl}/${String(pathname).replace(/^\/+/, '')}`, {
      ...options,
      headers: {
        'Content-Type': 'application/json',
        ...(options.headers || {}),
        'X-YTU-Token': this.runtime.authToken,
      },
    });
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      throw new Error(body.detail || `El servicio respondió HTTP ${response.status}.`);
    }
    const contentType = response.headers.get('content-type') || '';
    return contentType.includes('application/json') ? response.json() : response.text();
  }

  stop() {
    if (!this.process) return Promise.resolve();
    this.stopping = true;
    this.onState({ status: 'stopping' });
    const child = this.process;
    this.process = null;
    const exited = new Promise((resolve) => {
      if (child.exitCode !== null || child.signalCode !== null) resolve();
      else child.once('exit', () => resolve());
    });

    if (isWindows) {
      // The service shuts down gracefully when its stdin closes; FFmpeg and
      // separator children are removed with the tree if it lingers.
      child.stdin?.end();
    } else {
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch {
        child.kill('SIGTERM');
      }
    }

    const forceTimer = setTimeout(() => {
      if (child.exitCode !== null) return;
      if (isWindows) {
        void killProcessTree(child.pid);
        return;
      }
      try {
        process.kill(-child.pid, 'SIGKILL');
      } catch {
        child.kill('SIGKILL');
      }
    }, STOP_GRACE_MS);
    forceTimer.unref();

    this.logStream?.end();
    this.logStream = null;
    return exited.finally(() => clearTimeout(forceTimer));
  }
}
