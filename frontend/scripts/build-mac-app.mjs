import assert from 'node:assert/strict';
import { stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  findPackagedApp,
  requestedMacArchitecture,
} from './lib/packaged-app.mjs';
import { runCommand } from './lib/run-command.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const electronBuilder = path.join(
  frontendDir,
  'node_modules',
  '.bin',
  'electron-builder',
);

async function main() {
  const architecture = requestedMacArchitecture();
  const startedAt = Date.now();
  await runCommand(electronBuilder, [
    '--config',
    path.join(frontendDir, 'electron-builder.config.mjs'),
    '--mac',
    'dir',
    `--${architecture}`,
  ], {
    cwd: frontendDir,
    label: 'electron-builder',
  });
  const appPath = await findPackagedApp({
    frontendDir,
    desiredArch: architecture,
  });
  const details = await stat(path.join(appPath, 'Contents', 'Info.plist'));
  assert.ok(
    details.mtimeMs >= startedAt - 2_000,
    `electron-builder no actualizó el bundle esperado: ${appPath}`,
  );
  process.stdout.write(`[mac-app] ✓ ${appPath}\n`);
}

main().catch((error) => {
  process.stderr.write(`[mac-app] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
