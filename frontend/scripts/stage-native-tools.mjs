import { createHash } from 'node:crypto';
import { createReadStream, createWriteStream, existsSync, readFileSync } from 'node:fs';
import { copyFile, mkdir, rename, rm, chmod } from 'node:fs/promises';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';
import { createGunzip } from 'node:zlib';
import ffmpegPath from 'ffmpeg-static';
import ffprobePath from '@derhuerst/ffprobe-static';
import { stageFingerprintTool } from './stage-fingerprint-tool.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const targetPlatform = process.argv
  .find((argument) => argument.startsWith('--platform='))
  ?.slice('--platform='.length) || process.platform;
const stageDir = path.join(
  frontendDir,
  targetPlatform === 'win32' ? '.electron-bin-win' : '.electron-bin',
);
const licenseDir = path.join(stageDir, 'licenses');
const downloadDir = path.join(frontendDir, 'node_modules', '.cache', 'ytu-windows-tools');

// Same ffmpeg-static release as the macOS binaries, pinned by checksum.
const WINDOWS_TOOLS = Object.freeze({
  'ffmpeg-win32-x64.gz': '8883a3dffbd0a16cf4ef95206ea05283f78908dbfb118f73c83f4951dcc06d77',
  'ffprobe-win32-x64.gz': 'f309e6223ad89d2fe54bccd420a7709b66fd27540674e92309578ed491a43c8d',
  'win32-x64.LICENSE': '8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903',
});

const packageLicenses = [
  [path.join(frontendDir, 'node_modules', 'ffmpeg-static', 'LICENSE'), 'ffmpeg-static-LICENSE'],
  [
    path.join(frontendDir, 'node_modules', '@derhuerst', 'ffprobe-static', 'LICENSE'),
    'ffprobe-static-LICENSE',
  ],
];

async function stageBinary(source, name) {
  if (!source) throw new Error(`No se encontró el binario estático ${name}.`);
  const target = path.join(stageDir, name);
  await copyFile(source, target);
  await chmod(target, 0o755);
}

async function sha256(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

async function downloadWindowsTool(name) {
  const { 'binary-release-tag': release } = JSON.parse(
    readFileSync(path.join(frontendDir, 'node_modules', 'ffmpeg-static', 'package.json'), 'utf8'),
  )['ffmpeg-static'];
  const expected = WINDOWS_TOOLS[name];
  const target = path.join(downloadDir, `${release}-${name}`);
  if (existsSync(target) && (await sha256(target)) === expected) return target;

  const url = `https://github.com/eugeneware/ffmpeg-static/releases/download/${release}/${name}`;
  process.stdout.write(`[native:win] descargando ${url}\n`);
  await mkdir(downloadDir, { recursive: true });
  const response = await fetch(url);
  if (!response.ok || !response.body) {
    throw new Error(`No se pudo descargar ${url} (HTTP ${response.status}).`);
  }
  const partial = `${target}.part`;
  await pipeline(Readable.fromWeb(response.body), createWriteStream(partial));
  const actual = await sha256(partial);
  if (actual !== expected) {
    await rm(partial, { force: true });
    throw new Error(`SHA-256 inesperado para ${url}: ${actual}`);
  }
  await rename(partial, target);
  return target;
}

async function stageWindowsTools() {
  for (const tool of ['ffmpeg', 'ffprobe']) {
    const archive = await downloadWindowsTool(`${tool}-win32-x64.gz`);
    await pipeline(
      createReadStream(archive),
      createGunzip(),
      createWriteStream(path.join(stageDir, `${tool}.exe`)),
    );
  }
  const license = await downloadWindowsTool('win32-x64.LICENSE');
  await copyFile(license, path.join(licenseDir, 'ffmpeg-LICENSE'));
  await copyFile(license, path.join(licenseDir, 'ffprobe-LICENSE'));
}

async function stageHostTools() {
  await stageBinary(ffmpegPath, 'ffmpeg');
  await stageBinary(ffprobePath, 'ffprobe');
  const licenses = [
    [path.join(frontendDir, 'node_modules', 'ffmpeg-static', 'ffmpeg.LICENSE'), 'ffmpeg-LICENSE'],
    [
      path.join(frontendDir, 'node_modules', '@derhuerst', 'ffprobe-static', 'ffprobe.LICENSE'),
      'ffprobe-LICENSE',
    ],
  ];
  for (const [source, name] of licenses) {
    await copyFile(source, path.join(licenseDir, name));
  }
}

async function main() {
  await rm(stageDir, { recursive: true, force: true });
  await mkdir(licenseDir, { recursive: true });
  if (targetPlatform === 'win32') await stageWindowsTools();
  else await stageHostTools();
  await stageFingerprintTool(stageDir, targetPlatform);
  for (const [source, name] of packageLicenses) {
    await copyFile(source, path.join(licenseDir, name));
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
