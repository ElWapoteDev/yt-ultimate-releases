import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import windowsConfig from '../electron-builder.windows.config.mjs';
import { runCommand } from './lib/run-command.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const electronBuilder = path.join(
  frontendDir,
  'node_modules',
  '.bin',
  'electron-builder',
);

export async function windowsInstallerPath() {
  const { version } = JSON.parse(
    await readFile(path.join(frontendDir, 'package.json'), 'utf8'),
  );
  const name = windowsConfig.nsis.artifactName
    .replace('${productName}', windowsConfig.productName)
    .replace('${version}', version)
    .replace('${ext}', 'exe');
  return path.join(frontendDir, windowsConfig.directories.output, name);
}

async function main() {
  const startedAt = Date.now();
  await runCommand(electronBuilder, [
    '--config',
    path.join(frontendDir, 'electron-builder.windows.config.mjs'),
    '--win',
    'nsis',
    '--x64',
    // Releases are uploaded by scripts/publish-windows-release.mjs.
    '--publish',
    'never',
  ], {
    cwd: frontendDir,
    label: 'electron-builder',
  });
  const installer = await windowsInstallerPath();
  const details = await stat(installer);
  assert.ok(
    details.mtimeMs >= startedAt - 2_000,
    `electron-builder no actualizó el instalador esperado: ${installer}`,
  );
  process.stdout.write(`[win-installer] ✓ ${installer} (${details.size} bytes)\n`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`[win-installer] ✗ ${error.stack || error}\n`);
    process.exitCode = 1;
  });
}
