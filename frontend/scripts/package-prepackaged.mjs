import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import builderConfig from '../electron-builder.config.mjs';
import {
  findPackagedApp,
  requestedMacArchitecture,
} from './lib/packaged-app.mjs';
import { runCommand } from './lib/run-command.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const releaseDir = path.join(frontendDir, 'release');
const electronBuilder = path.join(
  frontendDir,
  'node_modules',
  '.bin',
  'electron-builder',
);

async function main() {
  const architecture = requestedMacArchitecture();
  const packageDocument = JSON.parse(
    await readFile(path.join(frontendDir, 'package.json'), 'utf8'),
  );
  const appPath = await findPackagedApp({
    frontendDir,
    desiredArch: architecture,
  });
  const appDetails = await stat(path.join(appPath, 'Contents', 'Info.plist'));
  process.stdout.write(`[prepackaged] usando ${appPath}\n`);
  await runCommand(electronBuilder, [
    '--config',
    path.join(frontendDir, 'electron-builder.config.mjs'),
    '--prepackaged',
    appPath,
    '--mac',
    'dmg',
    'zip',
    `--${architecture}`,
  ], {
    cwd: frontendDir,
    label: 'electron-builder',
  });

  for (const extension of ['.dmg', '.zip']) {
    const name = `${builderConfig.productName}-${packageDocument.version}`
      + `-${architecture}${extension}`;
    const artifact = await stat(path.join(releaseDir, name));
    assert.ok(artifact.isFile(), `No es un archivo regular: ${name}.`);
    assert.ok(artifact.size > 1_000_000, `${name} está incompleto.`);
    assert.ok(
      artifact.mtimeMs >= appDetails.mtimeMs - 2_000,
      `${name} es anterior al .app probado.`,
    );
    process.stdout.write(
      `[prepackaged] ✓ ${name} (${artifact.size} bytes)\n`,
    );
  }
}

main().catch((error) => {
  process.stderr.write(`[prepackaged] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
