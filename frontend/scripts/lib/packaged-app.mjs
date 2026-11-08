import assert from 'node:assert/strict';
import { access, readdir, realpath, stat } from 'node:fs/promises';
import path from 'node:path';

export function requestedMacArchitecture(
  value = process.env.YTU_ARCH || process.arch,
) {
  assert.ok(
    value === 'arm64' || value === 'x64',
    `Arquitectura macOS no soportada: ${value}`,
  );
  return value;
}

export function lipoArchitecture(architecture) {
  return requestedMacArchitecture(architecture) === 'x64'
    ? 'x86_64'
    : 'arm64';
}

export async function pathExists(candidate) {
  try {
    await access(candidate);
    return true;
  } catch {
    return false;
  }
}

export async function findPackagedApp({
  frontendDir,
  productName = 'YT Ultimate',
  configuredPath = process.env.YTU_PACKAGED_APP || '',
  desiredArch = requestedMacArchitecture(),
} = {}) {
  assert.ok(frontendDir, 'findPackagedApp requiere frontendDir.');
  const appBundleName = `${productName}.app`;
  const releaseDir = path.join(frontendDir, 'release');

  if (configuredPath) {
    const candidate = path.resolve(frontendDir, configuredPath);
    assert.equal(
      path.basename(candidate),
      appBundleName,
      `YTU_PACKAGED_APP debe apuntar a ${appBundleName}`,
    );
    return realpath(candidate);
  }

  let entries;
  try {
    entries = await readdir(releaseDir, { withFileTypes: true });
  } catch (error) {
    throw new Error(
      `No existe ${releaseDir}. Empaqueta primero con npm run desktop:build.`,
      { cause: error },
    );
  }

  const candidates = [];
  const direct = path.join(releaseDir, appBundleName);
  if (await pathExists(direct)) candidates.push({ path: direct, score: 40 });

  for (const entry of entries) {
    if (!entry.isDirectory() || !entry.name.startsWith('mac')) continue;
    const candidate = path.join(releaseDir, entry.name, appBundleName);
    if (!(await pathExists(candidate))) continue;
    let score = 10;
    if (entry.name === `mac-${desiredArch}`) score = 100;
    else if (entry.name.includes(desiredArch)) score = 90;
    else if (entry.name === 'mac') score = 50;
    const details = await stat(candidate);
    candidates.push({ path: candidate, score, mtimeMs: details.mtimeMs });
  }

  assert.ok(
    candidates.length,
    `No se encontró ${appBundleName} dentro de ${releaseDir}.`,
  );
  candidates.sort(
    (left, right) => right.score - left.score
      || (right.mtimeMs || 0) - (left.mtimeMs || 0),
  );
  return realpath(candidates[0].path);
}
