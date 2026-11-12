import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import iconGen from 'icon-gen';
import sharp from 'sharp';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(scriptDir, '..');
const buildDir = path.join(frontendDir, 'build');
const source = path.join(buildDir, 'icon-1024.png');
const dmgSource = path.join(buildDir, 'dmg-background-source.png');
const pngDir = path.join(buildDir, 'icon-png');
// Windows packaging uses its own resource folder so the installer does not
// pick up the macOS DMG license.
const windowsBuildDir = path.join(buildDir, 'windows');
const sizes = [16, 32, 64, 128, 256, 512, 1024];
const icoSizes = [16, 24, 32, 48, 64, 128, 256];

await mkdir(pngDir, { recursive: true });
await mkdir(windowsBuildDir, { recursive: true });
await Promise.all(
  [...new Set([...sizes, ...icoSizes])].map((size) =>
    sharp(source)
      .resize(size, size, { fit: 'contain' })
      .png()
      .toFile(path.join(pngDir, `${size}.png`)),
  ),
);

await iconGen(pngDir, buildDir, {
  icns: {
    name: 'icon',
    sizes,
  },
});
await iconGen(pngDir, windowsBuildDir, {
  ico: {
    name: 'icon',
    sizes: icoSizes,
  },
});

await sharp(dmgSource)
  .resize(660, 430, { fit: 'cover', position: 'centre' })
  .png()
  .toFile(path.join(buildDir, 'dmg-background.png'));

console.log(`Recursos de macOS y Windows generados en ${buildDir}`);
