import { cp, mkdir, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const backendDir = path.resolve(frontendDir, '..', 'backend');
const sourceDir = path.join(backendDir, 'build', 'pyinstaller', 'yt-ultimate-backend');
const stageDir = path.join(frontendDir, '.electron-backend');

async function main() {
  if (!existsSync(path.join(sourceDir, 'yt-ultimate-backend'))) {
    throw new Error(
      'Falta el sidecar compilado. Ejecuta `npm run backend:build` antes de empaquetar.',
    );
  }
  await rm(stageDir, { recursive: true, force: true });
  await mkdir(stageDir, { recursive: true });
  await cp(sourceDir, stageDir, {
    recursive: true,
    dereference: false,
    verbatimSymlinks: true,
    force: true,
    preserveTimestamps: true,
  });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
