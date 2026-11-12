import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import builderConfig from '../electron-builder.config.mjs';
import {
  findPackagedApp,
  requestedMacArchitecture,
} from './lib/packaged-app.mjs';

const execFileAsync = promisify(execFile);
const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const releaseDir = path.join(frontendDir, 'release');

async function run(executable, args, timeout = 180_000) {
  return execFileAsync(executable, args, {
    cwd: frontendDir,
    encoding: 'utf8',
    maxBuffer: 8 * 1024 * 1024,
    timeout,
  });
}

async function sha256(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

async function main() {
  assert.equal(process.platform, 'darwin', 'La verificación requiere macOS.');
  const architecture = requestedMacArchitecture();
  const packageDocument = JSON.parse(
    await readFile(path.join(frontendDir, 'package.json'), 'utf8'),
  );
  const basename = `${builderConfig.productName}-${packageDocument.version}`
    + `-${architecture}`;
  const dmg = path.join(releaseDir, `${basename}.dmg`);
  const zip = path.join(releaseDir, `${basename}.zip`);
  const appPath = await findPackagedApp({
    frontendDir,
    desiredArch: architecture,
  });

  const [appInfo, dmgInfo, zipInfo] = await Promise.all([
    stat(path.join(appPath, 'Contents', 'Info.plist')),
    stat(dmg),
    stat(zip),
  ]);
  for (const [label, details] of [['DMG', dmgInfo], ['ZIP', zipInfo]]) {
    assert.ok(details.isFile(), `${label} no es un archivo regular.`);
    assert.ok(details.size > 1_000_000, `${label} está incompleto.`);
    assert.ok(
      details.mtimeMs >= appInfo.mtimeMs - 2_000,
      `${label} es anterior al .app probado.`,
    );
  }

  await Promise.all([
    run('/usr/bin/hdiutil', ['verify', dmg]),
    run('/usr/bin/unzip', ['-tq', zip]),
    run('/usr/bin/codesign', [
      '--verify',
      '--deep',
      '--strict',
      '--verbose=2',
      appPath,
    ]),
  ]);
  const { stdout: resources } = await run(
    '/usr/bin/hdiutil',
    ['udifderez', '-xml', dmg],
  );
  assert.match(resources, /<key>LPic<\/key>/);
  assert.match(resources, /<key>TEXT<\/key>/);
  assert.match(resources, /<string>Spanish(?: SLA)?<\/string>/);

  const [dmgHash, zipHash] = await Promise.all([sha256(dmg), sha256(zip)]);
  process.stdout.write(
    `[artifacts] ✓ DMG válido (${dmgInfo.size} bytes) sha256=${dmgHash}\n`,
  );
  process.stdout.write(
    `[artifacts] ✓ ZIP válido (${zipInfo.size} bytes) sha256=${zipHash}\n`,
  );
}

main().catch((error) => {
  process.stderr.write(`[artifacts] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
