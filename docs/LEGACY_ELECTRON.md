# YT Ultimate

YT Ultimate es una aplicación de escritorio para macOS y Windows que reúne descarga,
biblioteca y reproducción de audio y vídeo en una sola experiencia. La interfaz
mantiene las decisiones comunes a la vista y reserva el procesamiento avanzado,
los diagnósticos y los plugins para quien decida activarlos.

![Icono de YT Ultimate](frontend/build/icon-1024.png)

## Qué incluye

- Búsqueda y reproducción desde YouTube, además de descargas de audio, vídeo y
  playlists.
- Biblioteca local indexada, playlists y trabajos persistentes.
- Importación desde Finder, selector nativo, asociaciones de archivo y
  arrastrar/soltar.
- Reproductor con cola reordenable, historial, aleatorio, repetición, velocidad,
  saltos de 10 segundos, temporizador, vídeo y Picture in Picture.
- Menús, atajos, menú contextual, documentos recientes y apertura en Finder
  integrados con macOS.
- En Windows: botones de ventana nativos, menú de la aplicación, Jump List,
  asociaciones de archivo, apertura en el Explorador y actualizaciones
  automáticas. Los menús contextuales y el menú ☰ se dibujan dentro de la
  ventana.
- Herramientas avanzadas para separar stems y crear variaciones de audio con
  comparación local de huellas Chromaprint. El modo **CALIDAD** separa la voz,
  modifica la instrumental con una ecualización dinámica y después aplica
  cambios suaves de tono, velocidad y volumen a la voz y la instrumental juntas.
  Guarda el resultado en FLAC de 24 bits.
  [Funcionamiento y fuentes](docs/FINGERPRINT.md).
- Modo desarrollador opt-in con Plugin API v1, configuración validada, comandos,
  eventos, diagnósticos y recarga explícita.

Los formatos reconocidos son MP3, MP4, M4A, WebM, MKV, WAV, FLAC, AAC, OGG y
Opus. También se admiten playlists M3U y M3U8 al abrir o importar archivos.

> Usa las funciones de descarga únicamente con contenido que tengas derecho a
> obtener y de acuerdo con las condiciones de la fuente.

## Uso cotidiano

La aplicación arranca y detiene su propio servicio local; una instalación normal
no exige configurar Python, puertos ni servidores.

1. Busca una canción o pega su enlace para reproducirla o descargarla.
2. Importa archivos desde **Archivo → Importar**, arrástralos a la ventana o abre
   un formato asociado desde Finder o el Explorador de Windows.
3. Haz clic derecho sobre una canción (en Inicio, Canciones o una playlist), un
   resultado, la cola o el reproductor para acceder a las acciones adecuadas al
   contexto; en los campos de texto ofrece Cortar, Copiar y Pegar. Arrastra una
   canción fuera de la ventana para copiar el archivo a otra carpeta o app.
4. Abre **Ajustes** con `⌘,` (`Ctrl+,` en Windows) para cambiar calidad,
   almacenamiento y preferencias. En Windows, el botón ☰ de la barra lateral
   abre el menú de la aplicación.

Las herramientas de procesamiento permanecen ocultas hasta activar
**Herramientas avanzadas**. El modo desarrollador es independiente y permanece
apagado de forma predeterminada.

**CALIDAD**, desde el perfil 6, eleva ligeramente el tono y acelera la canción
alrededor de un 1,6 %, manteniendo voz e instrumental sincronizadas. El volumen
se ajusta en conjunto para evitar saturación. La calidad se valora al escuchar
cada resultado: la voz también cambia y la separación por IA puede dejar
artefactos. Las copias históricas de los perfiles 4–5 conservan su descripción
de procesamiento solo instrumental. La coincidencia local de bits Chromaprint
se muestra aparte del reconocimiento externo: no indica un porcentaje de
protección ni demuestra que Audible Magic u otro detector deje de identificar
la canción.

## Preparar un entorno de desarrollo

El backend declara compatibilidad con Python 3.11 a 3.14. El empaquetado usa la
arquitectura del proceso de build (`arm64` o `x64`); `YTU_ARCH` permite
declararla explícitamente, pero debe coincidir con `process.arch`. Para crear un
paquete x64 desde Apple Silicon hay que ejecutar Node, Python y `npm ci` bajo
Rosetta, o usar un runner Intel. Esta validación evita mezclar Electron x64 con
sidecars arm64.

Desde la raíz del proyecto:

```bash
python3 -m venv backend/venv
backend/venv/bin/python -m pip install \
  -r backend/requirements.txt \
  -r backend/requirements-build.txt
cd frontend
npm ci
node scripts/stage-fingerprint-tool.mjs
npm run desktop:dev
```

`desktop:dev` levanta Vite, abre Electron y hace que Electron gestione el backend.
El entorno virtual debe estar en `backend/venv`, que es la ubicación esperada
por el controlador de desarrollo.

Para trabajar sólo en el renderer:

```bash
cd frontend
npm run dev
```

En ese modo debe existir un backend accesible. El renderer usa
`http://127.0.0.1:8000/api/v1` como valor de desarrollo, o las variables
`VITE_API_BASE_URL`, `VITE_API_ORIGIN` y `VITE_API_TOKEN` si se proporcionan.

## Comandos

Todos los comandos de escritorio se ejecutan desde `frontend/`.

| Comando | Propósito |
| --- | --- |
| `npm run desktop:dev` | Ejecutar renderer, Electron y backend en desarrollo. |
| `npm run lint` | Analizar el código JavaScript y React con ESLint. |
| `npm run build` | Compilar el renderer con Vite. |
| `npm run test:backend` | Ejecutar las pruebas unitarias y de contrato de Python. |
| `npm run test:e2e` | Ejecutar las pruebas headless de Electron con Playwright. |
| `npm run test:headless` | Ejecutar lint, build, backend y E2E en una sola barrera. |
| `npm run test:packaged` | Validar recursos, sidecars, arquitectura y arranque del `.app` ya ensamblado. |
| `npm run test:packaged:processing` | Separar un WAV sintético con FFmpeg, BS-RoFormer y los binarios del `.app`. |
| `npm run test:dev-processing` | Probar CALIDAD con BS-RoFormer real usando la configuración de arranque de la app en desarrollo. |
| `npm run test:artifacts` | Verificar DMG, ZIP, licencia, firma e imprimir sus SHA-256. |
| `npm run prepare:desktop` | Compilar y preparar sidecar, binarios, iconos y renderer. |
| `npm run desktop:build` | Probar todo y crear una aplicación macOS sin instalador. |
| `npm run desktop:dist` | Probar todo y producir DMG y ZIP firmables. |
| `npm run desktop:dist:unsigned` | Probar todo y producir DMG y ZIP sin descubrir firma. |
| `npm run prepare:desktop:win` | Preparar backend embebido, FFmpeg, iconos y renderer para Windows. |
| `npm run build:win-installer` | Crear el instalador NSIS de Windows con lo ya preparado. |
| `npm run test:win-package` | Verificar estructura, arquitectura y dependencias DLL del paquete de Windows. |
| `npm run desktop:dist:win` | Probar todo y producir el instalador `.exe` para Windows x64. |
| `npm run publish:win` | Publicar el instalador ya verificado como actualización en GitHub Releases. |
| `npm run release:win` | `desktop:dist:win` seguido de `publish:win`. |

Los comandos `desktop:build`, `desktop:dist` y `desktop:dist:unsigned` ejecutan
`test:headless` antes de preparar o empaquetar. Después ensamblan el `.app` y
ejecutan `test:packaged` y una inferencia real con
`test:packaged:processing`; sólo si ambos smoke tests terminan correctamente,
`desktop:dist` crea el DMG y el ZIP desde el bundle ya probado.
`prepare:desktop`, por sí solo, prepara recursos pero no sustituye esas barreras.

También se puede ejecutar el backend directamente:

```bash
cd backend
YTU_AUTH_TOKEN="token-local-de-desarrollo" venv/bin/python -m ytultimate
```

## Empaquetado para macOS

El flujo de build:

1. congela el backend y su motor de separación como sidecars con PyInstaller;
2. incorpora FFmpeg y FFprobe estáticos con sus licencias;
3. genera el icono ICNS y el fondo del DMG;
4. compila el renderer;
5. ensambla la aplicación, el modelo local y la documentación del SDK con
   electron-builder;
6. comprueba arquitectura, sidecars, recursos, backend autenticado, renderer de
   producción e importación real en un perfil temporal;
7. genera un WAV sintético con el FFmpeg empaquetado, ejecuta BS-RoFormer con el
   separador empaquetado y valida sus dos stems WAV;
8. crea DMG y ZIP desde ese mismo `.app` probado.

Los artefactos se escriben en `frontend/release/`. Para un release firmado,
configura una identidad de firma aceptada por electron-builder y usa
`YTU_RELEASE=1`; ese indicador hace obligatoria la firma y activa Hardened
Runtime. La configuración selecciona notarización cuando el release detecta el
conjunto `APPLE_API_KEY`,
`APPLE_API_KEY_ID` y `APPLE_API_ISSUER`, o el conjunto
`APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD` y `APPLE_TEAM_ID`. El flujo con API key
requiere las tres variables.
`YTU_APP_ID` permite reemplazar el identificador
`com.ytultimate.desktop` en infraestructura de distribución propia.

Ejemplo:

```bash
cd frontend
YTU_RELEASE=1 npm run desktop:dist
```

El comando sin firma existe para pruebas locales del paquete. No equivale a una
distribución notarizada ni evita las políticas de Gatekeeper.

## Instalador para Windows

El instalador de Windows x64 se genera desde macOS (o desde Windows) sin una
máquina Windows ni PyInstaller, porque PyInstaller no compila de forma cruzada.
Requiere Node, `npm ci` y [uv](https://docs.astral.sh/uv/):

```bash
cd frontend
npm run desktop:dist:win
```

El resultado es `frontend/release/YT-Ultimate-Setup-<versión>.exe`. Es un
instalador de un clic por usuario: no pide permisos de administrador, crea
accesos directos en el escritorio y el menú Inicio, registra los formatos
asociados y abre la aplicación al terminar. Funciona en Windows 10 y 11 de 64
bits sin instalar Python, FFmpeg ni Visual C++ por separado.

El flujo:

1. descarga el CPython 3.13 embebible oficial y le añade el runtime de Visual
   C++ junto a `python.exe`;
2. instala las dependencias para `win_amd64` desde
   `backend/requirements-windows.txt`, fijado con hashes a las mismas versiones
   que el entorno de macOS;
3. copia el backend, poda headers y suites de prueba y precompila el bytecode;
4. incorpora FFmpeg y FFprobe de la misma versión que en macOS;
5. ensambla el instalador NSIS con `electron-builder.windows.config.mjs`;
6. comprueba arquitectura, recursos, la configuración de actualizaciones y que
   cada `.exe`, `.dll` y `.pyd` encuentre todas sus DLL y símbolos.

En Windows el backend se ejecuta con `python -I -O`, igual que el sidecar de
macOS compilado con `optimize=1`, y se apaga de forma ordenada cuando Electron
cierra su entrada estándar.

### Actualizaciones automáticas

Desde la 4.2.0 la aplicación de Windows busca versiones nuevas en el repositorio
público [ElWapoteDev/yt-ultimate-releases](https://github.com/ElWapoteDev/yt-ultimate-releases),
que sólo contiene instaladores. Comprueba 20 segundos después de abrir y luego
cada 6 horas (también desde **Ayuda → Buscar actualizaciones…**), descarga en
segundo plano y pregunta si reiniciar para instalar; si se pospone, la
actualización se instala al cerrar la aplicación. Biblioteca, ajustes y plugins
se conservan. El registro queda en `%APPDATA%\YT Ultimate\logs\updater.log` y
`YTU_DISABLE_UPDATES=1` desactiva la comprobación.

Para publicar una versión, sube `version` en `frontend/package.json` (y en
`backend/pyproject.toml` y `backend/ytultimate/__init__.py`), inicia sesión con
`gh auth login` y ejecuta:

```bash
cd frontend
npm run release:win
```

`publish:win` crea el repositorio si no existe y sube el instalador, su
`.blockmap` (para descargas diferenciales) y `latest.yml` como release `v<versión>`.
Se niega a reemplazar una versión ya publicada.

Sin certificado de firma, SmartScreen muestra «Windows protegió tu PC» la
primera vez: hay que elegir **Más información → Ejecutar de todas formas**.
Para distribuirlo sin ese aviso, configura un certificado de firma de código
aceptado por electron-builder (`WIN_CSC_LINK` y `WIN_CSC_KEY_PASSWORD`).

## Diseño y seguridad

Electron elige un puerto loopback libre y genera un token aleatorio de 256 bits
en cada arranque. El token se entrega al renderer mediante un preload aislado y
se exige en todas las rutas `/api/v1`. El backend rechaza hosts que no sean
loopback, restringe CORS y `Host`, no publica la biblioteca como un directorio
estático y sirve contenido mediante identificadores validados.

El renderer se ejecuta con aislamiento de contexto, sin integración de Node y en
sandbox. El preload expone una superficie IPC pequeña para diálogos, menús,
Finder y drag-out. Consulta [Arquitectura](docs/ARCHITECTURE.md) para el mapa
completo y las decisiones de diseño.

## Modo desarrollador y plugins

Activar el modo desarrollador no habilita plugins nuevos automáticamente. Cada
plugin descubierto por primera vez comienza deshabilitado y requiere una segunda
confirmación para cargarse; los que el usuario ya haya habilitado conservan esa
elección. Los plugins son código Python confiable en el mismo proceso del
backend: los manifiestos, permisos declarativos, validaciones y timeouts ayudan a
evitar errores, pero **no constituyen una sandbox de seguridad**.

La guía práctica está en [Modo desarrollador](docs/DEVELOPER_MODE.md) y el
contrato de referencia en [Plugin SDK v1](backend/PLUGIN_SDK.md).
La aplicación incluye dos ejemplos auditables —uno básico y uno avanzado— que
pueden copiarse de forma no destructiva desde Ajustes. Se instalan deshabilitados
y nunca sustituyen una carpeta existente.

## Estructura del repositorio

```text
backend/
  ytultimate/            aplicación, API, servicios, persistencia y plugins
  tests/                 pruebas unitarias y contratos HTTP
  plugin-template/       plugin mínimo de referencia
  plugin-examples/       ejemplos básico y avanzado instalables
  yt-ultimate.spec       definición del sidecar PyInstaller
frontend/
  electron/              shell nativo, preload, IPC, menús y backend lifecycle
  src/                   renderer React y reproductor
  scripts/               build y staging reproducibles
  build/                 recursos del paquete macOS
docs/
  ARCHITECTURE.md        límites, flujo de datos y extensibilidad
  DEVELOPER_MODE.md      creación, activación y depuración de plugins
```

## Contribuir

Mantén las rutas HTTP delgadas, coloca las reglas de negocio en servicios,
inyecta dependencias mediante `Container` y conserva el renderer ajeno a rutas o
puertos fijos. Todo cambio funcional debe acompañarse de pruebas en la capa
correspondiente y pasar `npm run test:headless` antes de empaquetar.
