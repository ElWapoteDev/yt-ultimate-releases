import { copyFile, mkdir, rm, chmod } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const backendDir = path.resolve(frontendDir, '..', 'backend');
const python = process.env.YTU_BUILD_PYTHON
  || path.join(backendDir, 'venv', 'bin', 'python3');
const outputRoot = path.join(backendDir, 'build', 'pyinstaller');
const workRoot = path.join(backendDir, 'build', 'pyinstaller-work');
const cacheRoot = path.join(backendDir, 'build', 'pyinstaller-cache');
const spec = path.join(backendDir, 'yt-ultimate.spec');
const bundle = path.join(outputRoot, 'yt-ultimate-backend');
const executable = path.join(bundle, 'yt-ultimate-backend');
const separatorAlias = path.join(bundle, 'audio-separator');

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd || backendDir,
      env: options.env || process.env,
      stdio: 'inherit',
    });
    child.once('error', reject);
    child.once('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${path.basename(command)} terminó con código ${code}`));
    });
  });
}

async function main() {
  if (!existsSync(python)) {
    throw new Error(`No existe el Python de build: ${python}`);
  }

  await rm(outputRoot, { recursive: true, force: true });
  await rm(workRoot, { recursive: true, force: true });
  await mkdir(outputRoot, { recursive: true });
  await mkdir(workRoot, { recursive: true });
  await mkdir(cacheRoot, { recursive: true });

  await run(python, [
    '-m',
    'PyInstaller',
    '--noconfirm',
    '--clean',
    '--distpath',
    outputRoot,
    '--workpath',
    workRoot,
    spec,
  ], {
    env: {
      ...process.env,
      PYINSTALLER_CONFIG_DIR: cacheRoot,
    },
  });

  if (!existsSync(executable)) {
    throw new Error('PyInstaller no produjo yt-ultimate-backend.');
  }

  await copyFile(executable, separatorAlias);
  await chmod(executable, 0o755);
  await chmod(separatorAlias, 0o755);
  await run(executable, ['--version'], { cwd: bundle });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
