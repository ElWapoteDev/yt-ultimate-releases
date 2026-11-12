import { test, expect, _electron as electron } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import {
  access,
  mkdtemp,
  mkdir,
  readFile,
  realpath,
  rm,
  writeFile,
} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ffmpegPath = require('ffmpeg-static');
const ffprobePath = require('@derhuerst/ffprobe-static');
const frontendDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../..',
);

let electronApp;
let page;
let sandboxDir;
let userDataDir;
let libraryDir;
let fixturesDir;
let audioOnePath;
let audioTwoPath;
let videoPath;
const rendererErrors = [];

function createWaveBuffer({
  durationSeconds = 1.25,
  frequency = 440,
  sampleRate = 44_100,
  amplitude = 0.2,
} = {}) {
  const sampleCount = Math.floor(durationSeconds * sampleRate);
  const dataLength = sampleCount * 2;
  const buffer = Buffer.alloc(44 + dataLength);

  buffer.write('RIFF', 0, 'ascii');
  buffer.writeUInt32LE(36 + dataLength, 4);
  buffer.write('WAVE', 8, 'ascii');
  buffer.write('fmt ', 12, 'ascii');
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20);
  buffer.writeUInt16LE(1, 22);
  buffer.writeUInt32LE(sampleRate, 24);
  buffer.writeUInt32LE(sampleRate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write('data', 36, 'ascii');
  buffer.writeUInt32LE(dataLength, 40);

  for (let index = 0; index < sampleCount; index += 1) {
    const sample = Math.sin((2 * Math.PI * frequency * index) / sampleRate);
    buffer.writeInt16LE(Math.round(sample * amplitude * 32_767), 44 + index * 2);
  }
  return buffer;
}

async function createMediaFixtures() {
  audioOnePath = path.join(fixturesDir, 'Headless Uno.wav');
  audioTwoPath = path.join(fixturesDir, 'Headless Dos.wav');
  videoPath = path.join(fixturesDir, 'Headless Video.mp4');

  await Promise.all([
    writeFile(audioOnePath, createWaveBuffer({ frequency: 440 })),
    writeFile(audioTwoPath, createWaveBuffer({ frequency: 660 })),
  ]);

  execFileSync(
    ffmpegPath,
    [
      '-hide_banner',
      '-loglevel',
      'error',
      '-y',
      '-f',
      'lavfi',
      '-i',
      'testsrc2=size=320x180:rate=24',
      '-f',
      'lavfi',
      '-i',
      'sine=frequency=523.25:sample_rate=44100',
      '-t',
      '1.5',
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-c:a',
      'aac',
      '-movflags',
      '+faststart',
      '-shortest',
      videoPath,
    ],
    { stdio: 'pipe' },
  );
}

async function apiRequest(pathname, options = {}) {
  return page.evaluate(
    async ({ pathname: requestedPath, options: requestOptions }) => {
      const runtime = window.ytuDesktop?.runtime;
      const response = await fetch(
        `${runtime.apiBaseUrl}/${requestedPath.replace(/^\/+/, '')}`,
        {
          method: requestOptions.method || 'GET',
          headers: {
            ...(requestOptions.body
              ? { 'Content-Type': 'application/json' }
              : {}),
            'X-YTU-Token': runtime.authToken,
            ...(requestOptions.headers || {}),
          },
          body: requestOptions.body
            ? JSON.stringify(requestOptions.body)
            : undefined,
        },
      );
      const contentType = response.headers.get('content-type') || '';
      const body = contentType.includes('application/json')
        ? await response.json()
        : await response.text();
      return {
        ok: response.ok,
        status: response.status,
        headers: Object.fromEntries(response.headers.entries()),
        body,
      };
    },
    { pathname, options },
  );
}

async function clickNativeMenuItem(topLevelLabel, itemLabel) {
  return electronApp.evaluate(
    ({ Menu }, labels) => {
      const applicationMenu = Menu.getApplicationMenu();
      const topLevel = applicationMenu?.items.find(
        (item) => item.label === labels.topLevelLabel,
      );
      const menuItem = topLevel?.submenu?.items.find(
        (item) => item.label === labels.itemLabel,
      );
      if (!menuItem) return false;
      menuItem.click();
      return true;
    },
    { topLevelLabel, itemLabel },
  );
}

async function nativeMenuSnapshot() {
  return electronApp.evaluate(({ Menu }) => {
    const serialize = (menu) => (menu?.items || []).map((item) => ({
      label: item.label,
      enabled: item.enabled,
      visible: item.visible,
      checked: item.checked,
      submenu: item.submenu
        ? serialize(item.submenu)
        : [],
    }));
    return serialize(Menu.getApplicationMenu());
  });
}

async function waitForMediaMetadata(expectedType) {
  await expect
    .poll(
      () => page.locator('video.video-stage__media').evaluate((media) => ({
        duration: media.duration,
        readyState: media.readyState,
        currentSrc: media.currentSrc,
        videoWidth: media.videoWidth,
      })),
      { timeout: 20_000 },
    )
    .toEqual(expect.objectContaining({
      duration: expect.any(Number),
      readyState: expect.any(Number),
      currentSrc: expect.stringContaining('/api/v1/library/tracks/'),
      videoWidth: expect.any(Number),
    }));

  await expect.poll(
    () => page.locator('video.video-stage__media').evaluate(
      (media, mediaType) => (
        media.readyState >= 1
        && Number.isFinite(media.duration)
        && media.duration > 0
        && (mediaType !== 'video' || media.videoWidth > 0)
      ),
      expectedType,
    ),
    { timeout: 20_000 },
  ).toBe(true);

  const metadata = await page.locator('video.video-stage__media').evaluate(
    (media) => ({
      duration: media.duration,
      readyState: media.readyState,
      videoWidth: media.videoWidth,
    }),
  );
  expect(metadata.readyState).toBeGreaterThanOrEqual(1);
  expect(metadata.duration).toBeGreaterThan(0);
  if (expectedType === 'video') expect(metadata.videoWidth).toBeGreaterThan(0);
}

test.describe.serial('YT Ultimate desktop headless', () => {
  test.beforeAll(async () => {
    sandboxDir = await realpath(
      await mkdtemp(path.join(os.tmpdir(), 'ytu-e2e-')),
    );
    userDataDir = path.join(sandboxDir, 'user-data');
    libraryDir = path.join(sandboxDir, 'library');
    fixturesDir = path.join(sandboxDir, 'fixtures');
    await Promise.all([
      mkdir(userDataDir, { recursive: true }),
      mkdir(libraryDir, { recursive: true }),
      mkdir(fixturesDir, { recursive: true }),
    ]);
    await createMediaFixtures();

    electronApp = await electron.launch({
      args: ['.', '--headless'],
      cwd: frontendDir,
      env: {
        ...process.env,
        YTU_HEADLESS: '1',
        YTU_TEST_USER_DATA: userDataDir,
        YTU_LIBRARY_DIR: libraryDir,
        YTU_FFMPEG_PATH: ffmpegPath,
        YTU_FFPROBE_PATH: ffprobePath,
      },
      timeout: 120_000,
    });

    electronApp.process().stderr?.on('data', (chunk) => {
      const message = String(chunk);
      if (
        /(UnhandledPromiseRejection|uncaught exception|FATAL|backend terminó inesperadamente)/i
          .test(message)
      ) {
        rendererErrors.push(message);
      }
    });

    page = await electronApp.firstWindow();
    page.on('pageerror', (error) => rendererErrors.push(error.stack || error.message));
    page.on('console', (message) => {
      if (message.type() === 'error') rendererErrors.push(message.text());
    });
    await page.waitForURL(/^http:\/\/127\.0\.0\.1:5173(?:\/|$)/, {
      timeout: 30_000,
    });
    await page.waitForLoadState('domcontentloaded');
    try {
      await expect(page.locator('.app-root')).toBeVisible();
    } catch (error) {
      const diagnostics = {
        url: page.url(),
        title: await page.title().catch(() => ''),
        html: (await page.content().catch(() => '')).slice(0, 2_000),
        errors: rendererErrors,
      };
      throw new Error(
        `${error.message}\n\nStartup diagnostics:\n${JSON.stringify(diagnostics, null, 2)}`,
      );
    }
  });

  test.afterAll(async () => {
    await electronApp?.close().catch(() => {});
    if (sandboxDir) await rm(sandboxDir, { recursive: true, force: true });
  });

  test('arranca aislada, expone runtime autenticado y mantiene el modo simple', async () => {
    const runtimeCheck = await page.evaluate(async () => {
      const runtime = window.ytuDesktop?.runtime;
      const authenticated = await fetch(`${runtime.apiBaseUrl}/system/health`, {
        headers: { 'X-YTU-Token': runtime.authToken },
      });
      return {
        runtime,
        healthStatus: authenticated.status,
        health: await authenticated.json(),
        meta: await window.ytuDesktop.getMeta(),
      };
    });
    const unauthorized = await fetch(
      `${runtimeCheck.runtime.apiBaseUrl}/system/health`,
    );

    expect(runtimeCheck.runtime.isDesktop).toBe(true);
    expect(runtimeCheck.runtime.authToken).toMatch(/^[A-Za-z0-9_-]{40,}$/);
    expect(runtimeCheck.runtime.apiOrigin).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(runtimeCheck.healthStatus).toBe(200);
    expect(unauthorized.status).toBe(401);
    expect(runtimeCheck.health.status).toBe('ok');
    expect(runtimeCheck.health.instance_id).toBe(runtimeCheck.runtime.instanceId);
    expect(runtimeCheck.health.paths.library).toBe(libraryDir);
    expect(runtimeCheck.meta.isDesktop).toBe(true);

    await expect(page.getByRole('button', { name: 'Inicio', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Buscar', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Canciones', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Ajustes', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Stems', exact: true })).toHaveCount(0);
    await expect(page.getByText('Nada en reproducción')).toBeVisible();

    const menu = await nativeMenuSnapshot();
    expect(menu.map((item) => item.label)).toEqual(
      expect.arrayContaining([
        'YT Ultimate',
        'Archivo',
        'Edición',
        'Reproducción',
        'Visualización',
        'Ventana',
      ]),
    );
    expect(menu.map((item) => item.label)).toEqual(
      expect.arrayContaining([
        expect.stringMatching(/^(Ayuda|Help)$/),
      ]),
    );
    expect(menu.some((item) => item.label === 'Desarrollo')).toBe(false);
  });

  test('importa audio y vídeo, conserva las fuentes y sirve rangos autenticados', async () => {
    await page.evaluate(() => {
      const dataTransfer = new DataTransfer();
      dataTransfer.items.add(new File(
        [new Uint8Array([82, 73, 70, 70])],
        'arrastre-headless.wav',
        { type: 'audio/wav' },
      ));
      window.__ytuE2EDragTransfer = dataTransfer;
      window.dispatchEvent(new DragEvent('dragenter', {
        bubbles: true,
        cancelable: true,
        dataTransfer,
      }));
    });
    const dropZone = page.getByRole('status');
    await expect(dropZone).toContainText('Añadir a tu biblioteca');
    await expect(dropZone).toContainText(
      'Suelta canciones, vídeos, carpetas o playlists M3U',
    );
    await page.evaluate(() => {
      window.dispatchEvent(new DragEvent('dragleave', {
        bubbles: true,
        cancelable: true,
        dataTransfer: window.__ytuE2EDragTransfer,
      }));
      delete window.__ytuE2EDragTransfer;
    });
    await expect(dropZone).toHaveCount(0);

    const imported = await apiRequest('/library/import', {
      method: 'POST',
      body: {
        paths: [audioOnePath, audioTwoPath, videoPath],
        mode: 'copy',
        playlist_name: '',
      },
    });

    expect(imported.status).toBe(200);
    expect(imported.body.imported_count).toBe(3);
    expect(imported.body.errors).toEqual([]);
    expect(imported.body.imported).toHaveLength(3);

    await Promise.all([
      access(audioOnePath),
      access(audioTwoPath),
      access(videoPath),
    ]);
    const copiedNames = (await Promise.all(
      imported.body.imported.map(async (track) => {
        await access(track.local_path);
        expect(path.resolve(track.local_path).startsWith(path.resolve(libraryDir))).toBe(true);
        return path.basename(track.local_path);
      }),
    )).sort();
    expect(copiedNames).toEqual(
      ['Headless Dos.wav', 'Headless Uno.wav', 'Headless Video.mp4'].sort(),
    );

    const audioTrack = imported.body.imported.find((track) => track.type === 'audio');
    const ranged = await page.evaluate(async (track) => {
      const runtime = window.ytuDesktop.runtime;
      const url = new URL(track.path, runtime.apiOrigin);
      url.searchParams.set('token', runtime.authToken);
      const response = await fetch(url, {
        headers: { Range: 'bytes=0-31' },
      });
      return {
        status: response.status,
        contentRange: response.headers.get('content-range'),
        length: (await response.arrayBuffer()).byteLength,
      };
    }, audioTrack);
    expect(ranged.status).toBe(206);
    expect(ranged.contentRange).toMatch(/^bytes 0-31\/\d+$/);
    expect(ranged.length).toBe(32);

    await page.getByRole('button', { name: 'Canciones', exact: true }).click();
    await page.evaluate(() => {
      window.dispatchEvent(new CustomEvent('ytu:library-changed'));
    });
    await expect(page.getByRole('heading', { name: 'Canciones' })).toBeVisible();
    await expect(page.locator('.media-card')).toHaveCount(3);
    await expect(page.getByRole('button', { name: 'Headless Uno' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Headless Video' })).toBeVisible();
  });

  test('reproduce, controla velocidad desde el menú nativo y reordena la cola', async () => {
    await page.getByTitle('Reproducir en orden', { exact: true }).click();
    await expect(page.locator('.player-song')).toContainText('Headless');
    await waitForMediaMetadata('audio');

    await expect.poll(
      () => page.locator('video.video-stage__media').evaluate((media) => (
        !media.paused && media.readyState >= 2
      )),
      { timeout: 20_000 },
    ).toBe(true);

    const setRate = await electronApp.evaluate(({ Menu }) => {
      const playbackMenu = Menu.getApplicationMenu()?.items.find(
        (item) => item.label === 'Reproducción',
      );
      const rates = playbackMenu?.submenu?.items.find(
        (item) => item.label === 'Velocidad',
      );
      const rate = rates?.submenu?.items.find((item) => item.label === '1.5×');
      if (!rate) return false;
      rate.click();
      return true;
    });
    expect(setRate).toBe(true);
    await expect
      .poll(() => page.locator('video.video-stage__media').evaluate((media) => media.playbackRate))
      .toBe(1.5);

    expect(await clickNativeMenuItem('Visualización', 'Cola de reproducción')).toBe(true);
    const queuePanel = page.getByRole('dialog', { name: 'Cola de reproducción' });
    await expect(queuePanel).toBeVisible();
    const queuedRows = queuePanel.locator('.row[draggable="true"]');
    await expect(queuedRows).toHaveCount(2);

    const before = await queuedRows.locator('.row__title').allTextContents();
    await queuedRows.nth(0).dragTo(queuedRows.nth(1));
    await expect
      .poll(() => queuedRows.locator('.row__title').allTextContents())
      .toEqual([before[1], before[0]]);

    await page.keyboard.press('Escape');
    await expect(queuePanel).toBeHidden();
  });

  test('abre el reproductor de vídeo completo y vuelve a la interfaz', async () => {
    await page.getByRole('button', { name: 'Vídeo', exact: true }).click();
    const videoCard = page.getByRole('button', { name: 'Headless Video' });
    await expect(videoCard).toBeVisible();
    await videoCard.click();
    await expect(page.locator('.player-song')).toContainText('Headless Video');
    await waitForMediaMetadata('video');

    await page.getByTitle('Mostrar vídeo').click();
    const stage = page.getByRole('dialog', { name: 'Reproductor de vídeo' });
    await expect(stage).toBeVisible();
    await expect(stage.locator('video')).toBeVisible();
    await expect(stage.getByText('Headless Video')).toBeVisible();
    await stage.getByRole('button', { name: 'Cerrar' }).click();
    await expect(stage).toHaveCount(0);
  });

  test('abre menús contextuales nativos y en la página, y arrastra canciones como archivos', async () => {
    await page.getByRole('button', { name: 'Todo', exact: true }).click();
    const card = page.getByRole('button', { name: 'Headless Uno' });
    await expect(card).toBeVisible();
    const library = await apiRequest('/library');
    const songPath = library.body.find((track) => track.name === 'Headless Uno').local_path;
    const clipboardText = () => electronApp.evaluate(({ clipboard }) => clipboard.readText());

    // macOS: native menus, captured instead of shown.
    await electronApp.evaluate(({ Menu }) => {
      globalThis.ytuE2EPopups = [];
      globalThis.ytuE2EPopup = Menu.prototype.popup;
      Menu.prototype.popup = function capturePopup() {
        globalThis.ytuE2EPopups.push(this);
      };
      process.env.YTU_MENU_STYLE = 'native';
    });
    try {
      await card.click({ button: 'right' });
      await expect.poll(() => electronApp.evaluate(() => globalThis.ytuE2EPopups.length)).toBe(1);
      const nativeLabels = await electronApp.evaluate(
        () => globalThis.ytuE2EPopups[0].items.map((item) => item.label),
      );
      expect(nativeLabels).toEqual(expect.arrayContaining([
        'Reproducir', 'Agregar a la cola', 'Añadir a playlist', 'Obtener información',
        'Copiar ruta', 'Eliminar de la biblioteca…', 'Versión Alvin y las ardillas…',
      ]));
      await electronApp.evaluate(({ clipboard }) => {
        clipboard.clear();
        globalThis.ytuE2EPopups[0].items.find((item) => item.label === 'Copiar ruta').click();
      });
      await expect.poll(clipboardText).toBe(songPath);

      const filter = page.getByPlaceholder('Filtrar canciones');
      await filter.click({ button: 'right' });
      await expect.poll(() => electronApp.evaluate(() => globalThis.ytuE2EPopups.length)).toBe(2);
      expect(await electronApp.evaluate(
        () => globalThis.ytuE2EPopups[1].items.map((item) => item.label),
      )).toEqual(['Cortar', 'Copiar', 'Pegar', '', 'Seleccionar todo']);

      // Windows: the same menus drawn in the page.
      await electronApp.evaluate(({ clipboard }) => {
        clipboard.clear();
        process.env.YTU_MENU_STYLE = 'page';
      });
      await card.click({ button: 'right' });
      const menu = page.getByRole('menu').first();
      await expect(menu.getByRole('menuitem', { name: 'Eliminar de la biblioteca…' })).toBeVisible();
      await menu.getByRole('menuitem', { name: 'Añadir a playlist' }).hover();
      await expect(page.getByRole('menu').nth(1).getByRole('menuitem', { name: 'Nueva playlist…' }))
        .toBeVisible();
      await page.keyboard.press('Escape');
      await expect(page.getByRole('menu')).toHaveCount(0);

      await card.click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Copiar ruta' }).click();
      await expect(page.getByRole('menu')).toHaveCount(0);
      await expect.poll(clipboardText).toBe(songPath);

      await filter.fill('Headless');
      await filter.selectText();
      await filter.click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Copiar', exact: true }).click();
      await expect.poll(clipboardText).toBe('Headless');
      await filter.fill('');
      await filter.click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Pegar' }).click();
      await expect(filter).toHaveValue('Headless');
      await filter.fill('');
      expect(await electronApp.evaluate(() => globalThis.ytuE2EPopups.length)).toBe(2);

      // The Windows title bar menu runs the application menu items by id.
      const appMenu = await page.evaluate(() => window.ytuDesktop.getAppMenu());
      const searchItem = appMenu
        .find((item) => item.label === 'Visualización')
        .submenu.find((item) => item.label === 'Buscar');
      expect(searchItem.accelerator).toBe('CmdOrCtrl+2');
      await page.evaluate(
        (item) => window.ytuDesktop.clickAppMenuItem({ id: item.id, label: 'Otro elemento' }),
        searchItem,
      );
      await expect(page.getByRole('heading', { name: 'Canciones' })).toBeVisible();
      await page.evaluate((item) => window.ytuDesktop.clickAppMenuItem(item), searchItem);
      await expect(page.getByPlaceholder('Canciones, artistas o enlaces de YouTube')).toBeVisible();
      await page.getByRole('button', { name: 'Canciones', exact: true }).click();
      await expect(card).toBeVisible();
    } finally {
      await electronApp.evaluate(({ Menu }) => {
        Menu.prototype.popup = globalThis.ytuE2EPopup;
        delete process.env.YTU_MENU_STYLE;
      });
    }

    // Dragging out hands the file to the system and cancels the HTML drag.
    await electronApp.evaluate(({ BrowserWindow }) => {
      const contents = BrowserWindow.getAllWindows()[0].webContents;
      globalThis.ytuE2EDrags = [];
      contents.startDrag = (item) => {
        globalThis.ytuE2EDrags.push({ file: item.file, hasIcon: !item.icon.isEmpty() });
      };
    });
    try {
      const cancelled = await card.evaluate((element) => !element.dispatchEvent(new DragEvent(
        'dragstart',
        { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() },
      )));
      expect(cancelled).toBe(true);
      await expect.poll(() => electronApp.evaluate(() => globalThis.ytuE2EDrags))
        .toEqual([{ file: songPath, hasIcon: true }]);

      const dropZone = page.locator('.import-drop-zone');
      const fileDrag = (type) => page.evaluate((eventType) => {
        window.ytuE2EOutgoing ||= new DataTransfer();
        if (!window.ytuE2EOutgoing.items.length) {
          window.ytuE2EOutgoing.items.add(new File([new Uint8Array([1])], 'Headless Uno.wav'));
        }
        window.dispatchEvent(new DragEvent(eventType, {
          bubbles: true,
          cancelable: true,
          dataTransfer: window.ytuE2EOutgoing,
        }));
      }, type);
      await fileDrag('dragenter');
      await expect(dropZone).toHaveCount(0);
      await fileDrag('dragleave');
      // Once it left the window, an incoming file drag offers the import again.
      await fileDrag('dragenter');
      await expect(dropZone).toBeVisible();
      await fileDrag('dragleave');
      await expect(dropZone).toHaveCount(0);
    } finally {
      await electronApp.evaluate(({ BrowserWindow }) => {
        delete BrowserWindow.getAllWindows()[0].webContents.startDrag;
      });
    }
  });

  test('modifica audio y muestra el resultado real de la verificación de fingerprint', async () => {
    await apiRequest('/settings', { method: 'PATCH', body: { show_advanced_tools: true } });
    await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:settings-changed')));
    await expect(page.getByRole('button', { name: 'Stems', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Todo', exact: true }).click();
    await electronApp.evaluate(() => { process.env.YTU_MENU_STYLE = 'page'; });
    try {
      await page.getByRole('button', { name: 'Headless Uno' }).click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Modificar fingerprint', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Intenso · FLAC', exact: true }).click();
      let job;
      await expect.poll(async () => {
        job = (await apiRequest('/fingerprint/jobs')).body.at(-1);
        return job?.status;
      }, { timeout: 30_000 }).toBe('completed');
      expect(job.output.fingerprint_verification.pcm_changed).toBe(true);
      expect(job.output.fingerprint_profile.intensity).toBe('strong');
      expect(job.output.quality).toBe('FLAC');
      expect(job.output.fingerprint_verification.status).toBe('insufficient_audio');
      await expect(page.getByText(/fragmento es demasiado corto para comparar huellas/)).toBeVisible();
      const copy = page.getByRole('button', { name: 'Headless Uno - Protected' });
      await expect(copy).toBeVisible();
      await copy.click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Obtener información' }).click();
      await expect(page.locator('.modal-panel')).toContainText('demasiado corto');
      await expect(page.locator('.modal-panel')).toContainText('no garantiza');
      await expect(page.locator('.modal-panel')).toContainText('Reconocimiento externo');
      await expect(page.locator('.modal-panel')).toContainText('Sin comprobar.');
      await page.locator('.modal-panel').getByRole('button', { name: 'Cerrar' }).click();
      // Keep subsequent playback tests focused on the original fixture.
      await apiRequest(`/library/${job.output.track_id}`, { method: 'DELETE' });
      await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:library-changed')));
      await expect(copy).toHaveCount(0);
    } finally {
      await electronApp.evaluate(() => { delete process.env.YTU_MENU_STYLE; });
      await apiRequest('/settings', { method: 'PATCH', body: { show_advanced_tools: false } });
      await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:settings-changed')));
      await expect(page.getByRole('button', { name: 'Stems', exact: true })).toHaveCount(0);
    }
  });

  test('CALIDAD muestra la separación y los ajustes conjuntos del perfil 6', async () => {
    await page.getByRole('button', { name: 'Canciones', exact: true }).click();
    await apiRequest('/settings', { method: 'PATCH', body: { show_advanced_tools: true } });
    await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:settings-changed')));
    await expect(page.getByRole('button', { name: 'Stems', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Todo', exact: true }).click();
    const source = (await apiRequest('/library')).body.find((track) => track.name === 'Headless Uno');
    let submitted;
    let phase = 'separating';
    const qualityProfile = {
      version: 6, intensity: 'quality', vocals_preserved: false,
      processing_target: 'instrumental_then_full_mix',
      vocal_processing: 'shared_smooth_remap',
      pitch_semitones: 0.28, pitch_range_semitones: [0.20, 0.36], speed: 1.016,
    };
    const verification = { status: 'changed', recognition_tested: false };
    // Backend audio tests cover the actual mix; this test controls long-running
    // stages to check the menu request and the renderer's progress independently.
    const jobRoute = /\/fingerprint\/jobs(?:\/quality-ui-test)?$/;
    await page.route(jobRoute, async (route) => {
      if (route.request().method() === 'POST') {
        submitted = route.request().postDataJSON();
        await route.fulfill({ json: { id: 'quality-ui-test', status: 'queued' } });
      } else if (route.request().url().endsWith('/quality-ui-test')) {
        await route.fulfill({ json: {
          id: 'quality-ui-test',
          status: phase === 'completed' ? 'completed' : 'running',
          phase,
          progress: 50,
          output: phase === 'completed' ? {
            fingerprint_profile: qualityProfile,
            fingerprint_verification: verification,
          } : null,
        } });
      } else {
        await route.continue();
      }
    });
    const libraryRoute = /\/library$/;
    await page.route(libraryRoute, async (route) => {
      const response = await route.fetch();
      const tracks = await response.json();
      await route.fulfill({ response, json: tracks.map((track) => (
        phase === 'completed' && track.id === source.id ? {
          ...track,
          metadata: {
            ...track.metadata,
            fingerprint_profile: qualityProfile,
            fingerprint_verification: verification,
          },
        } : track
      )) });
    });
    await electronApp.evaluate(() => { process.env.YTU_MENU_STYLE = 'page'; });
    try {
      await page.getByRole('button', { name: 'Headless Uno' }).click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Modificar fingerprint', exact: true }).click();
      await page.getByRole('menuitem', { name: 'CALIDAD · FLAC', exact: true }).click();
      await expect.poll(() => submitted).toMatchObject({ intensity: 'quality', track_id: source.id });
      await expect(page.getByText('Separando voz e instrumental… 50%', { exact: true })).toBeVisible();
      phase = 'processing';
      await expect(page.getByText('Modificando instrumental… 50%', { exact: true })).toBeVisible();
      phase = 'recombining';
      await expect(page.getByText('Uniendo voz e instrumental… 50%', { exact: true })).toBeVisible();
      phase = 'remapping';
      await expect(page.getByText('Ajustando voz e instrumental… 50%', { exact: true })).toBeVisible();
      phase = 'verifying';
      await expect(page.getByText('Comparando huellas… 50%', { exact: true })).toBeVisible();
      phase = 'completed';
      await expect(page.getByText(/Variación creada\. Huella Chromaprint distinta/)).toBeVisible();
      await expect(page.getByText(/Reconocimiento externo sin comprobar/)).toBeVisible();
      await page.getByRole('button', { name: 'Headless Uno' }).click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Obtener información' }).click();
      const info = page.locator('.modal-panel');
      await expect(info).toContainText('CALIDAD · cambios suaves en voz e instrumental');
      await expect(info).toContainText('Voz e instrumental reciben juntas cambios suaves de tono, velocidad y volumen.');
      await expect(info).not.toContainText('Voz separada comprobada');
      await expect(info).toContainText('Sin comprobar.');
      await info.getByRole('button', { name: 'Cerrar' }).click();
    } finally {
      await page.unroute(jobRoute);
      await page.unroute(libraryRoute);
      await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:library-changed')));
      await electronApp.evaluate(() => { delete process.env.YTU_MENU_STYLE; });
      await apiRequest('/settings', { method: 'PATCH', body: { show_advanced_tools: false } });
      await page.evaluate(() => window.dispatchEvent(new CustomEvent('ytu:settings-changed')));
      await expect(page.getByRole('button', { name: 'Stems', exact: true })).toHaveCount(0);
    }
  });

  test('crea una versión Alvin y las ardillas con parámetros ajustables', async () => {
    await page.getByRole('button', { name: 'Todo', exact: true }).click();
    const card = page.getByRole('button', { name: 'Headless Uno' });
    await expect(card).toBeVisible();
    const source = (await apiRequest('/library')).body.find((track) => track.name === 'Headless Uno');

    await electronApp.evaluate(() => { process.env.YTU_MENU_STYLE = 'page'; });
    try {
      await card.click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Versión Alvin y las ardillas…' }).click();
    } finally {
      await electronApp.evaluate(() => { delete process.env.YTU_MENU_STYLE; });
    }
    const dialog = page.getByRole('dialog', { name: 'Versión Alvin y las ardillas' });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByText('+7 semitonos')).toBeVisible();
    await dialog.getByRole('button', { name: 'Súper aguda' }).click();
    await expect(dialog.getByText('+12 semitonos')).toBeVisible();
    await dialog.getByRole('slider').nth(1).fill('1.5');
    await expect(dialog.getByText('1.50×')).toBeVisible();
    await dialog.getByRole('button', { name: 'Crear versión' }).click();
    await expect(dialog).toHaveCount(0);

    let job;
    await expect.poll(async () => {
      job = (await apiRequest('/chipmunk/jobs')).body.at(-1);
      return job?.status;
    }, { timeout: 30_000 }).toBe('completed');
    expect(job.output.playlist).toBe('Ardillas');
    expect(job.output.chipmunk_profile).toEqual({ pitch_semitones: 12, speed: 1.5, brightness_db: 5 });
    expect(path.dirname(job.output.local_path)).toBe(path.join(libraryDir, 'Ardillas'));
    await access(job.output.local_path);
    expect(job.output.duration).toBeCloseTo(source.duration / 1.5, 0);
    await expect(page.getByText('Headless Uno (Ardilla)').first()).toBeVisible({ timeout: 10_000 });
  });

  test('habilita opciones avanzadas, activa desarrollo y crea un plugin funcional', async () => {
    await page.getByRole('button', { name: 'Ajustes', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Ajustes' })).toBeVisible();

    const disclosure = page.locator('details.settings-disclosure');
    if (!(await disclosure.getAttribute('open'))) {
      await disclosure.locator('summary').click();
    }

    const advancedToggle = page.getByRole('switch', {
      name: /Herramientas avanzadas/,
    });
    await advancedToggle.click();
    await expect(advancedToggle).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByRole('button', { name: 'Stems', exact: true })).toBeVisible();

    page.once('dialog', (dialog) => dialog.accept());
    const developerToggle = page.getByRole('switch', {
      name: /Modo desarrollador/,
    });
    await developerToggle.click();
    await expect(developerToggle).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByText('Código local de confianza · API v1')).toBeVisible();

    const bundledExamples = page.locator('.plugin-example-card');
    await expect(page.getByText('Ejemplos incluidos')).toBeVisible();
    await expect(bundledExamples).toHaveCount(2);
    const basicExample = bundledExamples.filter({
      hasText: 'Saludador básico',
    });
    await expect(basicExample).toContainText('Básico');
    await basicExample.getByRole('button', { name: 'Instalar' }).click();
    await expect(
      basicExample.getByRole('button', { name: 'Instalado' }),
    ).toBeDisabled();

    const basicPluginCard = page.locator('.plugin-card').filter({
      hasText: 'Saludador básico',
    });
    await expect(basicPluginCard).toContainText('Inactivo');
    await access(path.join(
      userDataDir,
      'Plugins',
      'dev.ytultimate.basic-greeter',
      'plugin.json',
    ));
    await basicPluginCard.getByRole('switch', {
      name: 'Activar Saludador básico',
    }).click();
    await expect(basicPluginCard).toContainText('Activo');

    const configured = await apiRequest(
      '/developer/plugins/dev.ytultimate.basic-greeter/config',
      {
        method: 'PATCH',
        body: {
          config: {
            greeting: 'Qué tal',
            punctuation: '✨',
            uppercase: true,
          },
          replace: true,
        },
      },
    );
    expect(configured.status).toBe(200);
    const greeting = await apiRequest(
      '/developer/plugins/dev.ytultimate.basic-greeter/commands/greet',
      {
        method: 'POST',
        body: { arguments: { name: 'Ada' } },
      },
    );
    expect(greeting.status).toBe(200);
    expect(greeting.body.result.message).toBe('QUÉ TAL, ADA✨');

    await page.getByTitle('Crear plugin').click();
    const scaffold = page.locator('.plugin-scaffold');
    await expect(scaffold).toBeVisible();
    await scaffold.getByPlaceholder('mi.plugin').fill('e2e.headless');
    await scaffold.getByPlaceholder('Mi plugin').fill('Plugin Headless');
    await scaffold.getByPlaceholder('Qué hace este plugin').fill(
      'Verifica el SDK desde Electron headless.',
    );
    await scaffold.getByRole('button', { name: 'Crear', exact: true }).click();

    const pluginCard = page.locator('.plugin-card').filter({
      hasText: 'Plugin Headless',
    });
    await expect(pluginCard).toBeVisible();
    await expect(pluginCard).toContainText('Inactivo');
    await access(path.join(userDataDir, 'Plugins', 'e2e.headless', 'plugin.json'));
    const manifest = JSON.parse(await readFile(
      path.join(userDataDir, 'Plugins', 'e2e.headless', 'plugin.json'),
      'utf8',
    ));
    expect(manifest).toMatchObject({
      manifest_version: 1,
      id: 'e2e.headless',
      api_version: 1,
      entrypoint: 'plugin.py:setup',
    });

    await pluginCard.getByRole('switch', { name: 'Activar Plugin Headless' }).click();
    await expect(pluginCard).toContainText('Activo');

    const command = await apiRequest(
      '/developer/plugins/e2e.headless/commands/greet',
      {
        method: 'POST',
        body: { arguments: { name: 'Codex' } },
      },
    );
    expect(command.status).toBe(200);
    expect(command.body.result).toEqual({ message: 'Hola, Codex!' });

    await expect
      .poll(async () => {
        const menu = await nativeMenuSnapshot();
        return menu.map((item) => item.label);
      })
      .toContain('Desarrollo');
  });

  test('no produjo excepciones del renderer durante el flujo', async () => {
    expect(rendererErrors).toEqual([]);
  });
});
