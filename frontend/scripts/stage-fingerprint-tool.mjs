import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

// Official release assets and SHA-256 digests from the GitHub release API.
const VERSION = '1.6.2';
const ASSETS = {
  darwin: ['macos-universal.tar.gz', '53d9368f96017fb288d74f6ed4d928f7bbbfb2ff81e69b4ef67b6ceeb217d9b2'],
  win32: ['windows-x86_64.zip', 'b74369726676670af3ab0eadff45f960cf969fdf4a87a971ba37eaf8d530e874'],
};
const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export async function stageFingerprintTool(stageDir, platform) {
  const asset = ASSETS[platform];
  if (!asset) throw new Error(`Chromaprint: unsupported platform ${platform}`);
  const [suffix, digest] = asset;
  const name = `chromaprint-fpcalc-${VERSION}-${suffix}`;
  const cache = path.join(frontendDir, 'node_modules', '.cache', 'ytu-chromaprint');
  await mkdir(cache, { recursive: true });
  const archive = path.join(cache, name);
  let bytes = await readFile(archive).catch(() => null);
  const valid = (value) => value && createHash('sha256').update(value).digest('hex') === digest;
  if (!valid(bytes)) {
    const response = await fetch(`https://github.com/acoustid/chromaprint/releases/download/v${VERSION}/${name}`);
    if (!response.ok) throw new Error(`Chromaprint download failed: ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    if (!valid(bytes)) throw new Error('Chromaprint SHA-256 mismatch');
    await writeFile(archive, bytes);
  }
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'ytu-chromaprint-'));
  try {
    execFileSync('tar', ['-xf', archive, '-C', temporary]);
    const directory = name.replace(/\.(tar\.gz|zip)$/, '');
    const binary = platform === 'win32' ? 'fpcalc.exe' : 'fpcalc';
    await mkdir(stageDir, { recursive: true });
    await copyFile(path.join(temporary, directory, binary), path.join(stageDir, binary));
    await chmod(path.join(stageDir, binary), 0o755);
    const licenses = path.join(stageDir, 'licenses');
    await mkdir(licenses, { recursive: true });
    for (const file of ['chromaprint-LICENSE.md', 'chromaprint-LGPL-2.1.txt', 'chromaprint-NOTICE.txt']) {
      await copyFile(path.join(frontendDir, 'licenses', file), path.join(licenses, file));
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const platform = process.argv.find((arg) => arg.startsWith('--platform='))?.split('=')[1] || process.platform;
  await stageFingerprintTool(path.join(frontendDir, platform === 'win32' ? '.electron-bin-win' : '.electron-bin'), platform);
}
