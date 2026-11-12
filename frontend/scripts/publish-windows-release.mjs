import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { releasesRepository } from '../electron-builder.windows.config.mjs';
import { windowsInstallerPath } from './build-windows-installer.mjs';
import { pathExists } from './lib/packaged-app.mjs';
import { runCommand } from './lib/run-command.mjs';
import { readUpdateYaml, sha512Base64 } from './lib/update-info.mjs';

// Uploads the verified Windows installer to the public repository installed
// copies check for updates (`publish` in electron-builder.windows.config.mjs).
// Uses the GitHub CLI session, which needs push access to that repository.
const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const repository = `${releasesRepository.owner}/${releasesRepository.repo}`;
const execFileAsync = promisify(execFile);

function report(message) {
  process.stdout.write(`[publish-win] ${message}\n`);
}

async function gh(args) {
  const { stdout } = await execFileAsync('gh', args, { maxBuffer: 16 * 1024 * 1024 });
  return stdout;
}

async function existingRelease(tag) {
  try {
    return JSON.parse(await gh(['release', 'view', tag, '--repo', repository, '--json', 'isDraft,url']));
  } catch {
    return null;
  }
}

function releaseNotes(version, installerName) {
  return [
    `YT Ultimate ${version} para Windows 10 y 11 (64 bits).`,
    '',
    `**Instalación nueva:** descarga \`${installerName}\` y ábrelo. Si Windows SmartScreen `
      + 'muestra un aviso, elige «Más información» y luego «Ejecutar de todas formas».',
    '',
    '**Ya instalada (4.2.0 o posterior):** la aplicación descarga esta versión sola y pregunta '
      + 'si quieres reiniciar para instalarla. Tu biblioteca y ajustes se conservan.',
  ].join('\n');
}

async function main() {
  const { version } = JSON.parse(await readFile(path.join(frontendDir, 'package.json'), 'utf8'));
  const installer = await windowsInstallerPath();
  const blockmap = `${installer}.blockmap`;
  const latest = path.join(path.dirname(installer), 'latest.yml');
  for (const file of [installer, blockmap, latest]) {
    assert.ok(await pathExists(file), `Falta ${file}. Ejecuta npm run desktop:dist:win`);
  }
  const updateInfo = await readUpdateYaml(latest);
  assert.equal(updateInfo.version, version, 'latest.yml es de otra versión; vuelve a generar el instalador');
  assert.equal(updateInfo.path, path.basename(installer), 'latest.yml apunta a otro archivo');
  assert.equal(
    updateInfo.sha512,
    await sha512Base64(installer),
    'latest.yml no coincide con el instalador; vuelve a generarlo',
  );

  try {
    await gh(['repo', 'view', repository, '--json', 'name']);
  } catch {
    report(`creando el repositorio público ${repository}`);
    // Releases need a commit to tag, so the repository starts with a README.
    await runCommand('gh', [
      'repo', 'create', repository,
      '--public',
      '--add-readme',
      '--description', 'Instaladores y actualizaciones automáticas de YT Ultimate para Windows',
    ], { label: 'gh repo create' });
  }

  const tag = `v${version}`;
  const assets = [installer, blockmap, latest];
  const release = await existingRelease(tag);
  if (!release) {
    report(`subiendo ${tag} a ${repository} (${assets.map((file) => path.basename(file)).join(', ')})`);
    // gh keeps the release as a draft until every asset finished uploading.
    await runCommand('gh', [
      'release', 'create', tag, ...assets,
      '--repo', repository,
      '--title', `YT Ultimate ${version}`,
      '--notes', releaseNotes(version, path.basename(installer)),
      '--latest',
    ], { label: 'gh release create' });
  } else if (release.isDraft) {
    // An interrupted upload leaves a draft that installed copies cannot see.
    report(`completando el borrador ${tag}`);
    await runCommand('gh', ['release', 'upload', tag, ...assets, '--repo', repository, '--clobber'], {
      label: 'gh release upload',
    });
    await runCommand('gh', ['release', 'edit', tag, '--repo', repository, '--draft=false', '--latest'], {
      label: 'gh release edit',
    });
  } else {
    throw new Error(
      `${tag} ya está publicada (${release.url}). Sube la versión en package.json antes de publicar otra.`,
    );
  }

  const published = await existingRelease(tag);
  assert.ok(published && !published.isDraft, `${tag} no quedó publicada`);
  report(`✓ ${published.url}`);
}

main().catch((error) => {
  process.stderr.write(`[publish-win] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
