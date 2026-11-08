# Renderer y shell de escritorio

Este directorio contiene la aplicación macOS de YT Ultimate: un renderer React
compilado con Vite, el proceso principal de Electron y el pipeline de
electron-builder. El backend Python no se copia como entorno virtual; se congela
como sidecar reproducible antes del empaquetado.

La visión general del proyecto está en [el README raíz](../README.md) y las
fronteras entre procesos están detalladas en
[Arquitectura](../docs/ARCHITECTURE.md).

## Desarrollo

Instala primero `backend/venv` y sus dependencias, después:

```bash
npm ci
npm run desktop:dev
```

`desktop:dev` ejecuta:

- Vite en `127.0.0.1:5173`;
- Electron con recarga del renderer;
- el backend desde `../backend/venv/bin/python -m ytultimate`.

Electron reserva un puerto local libre, genera el token de sesión y lo entrega al
renderer. Por eso éste es el modo recomendado para desarrollar la aplicación
completa.

`npm run dev` arranca únicamente Vite. En ese caso el backend y su autenticación
deben configurarse por separado mediante:

```text
VITE_API_BASE_URL=http://127.0.0.1:8000/api/v1
VITE_API_ORIGIN=http://127.0.0.1:8000
VITE_API_TOKEN=...
```

## Organización

```text
electron/
  main.mjs                ciclo de vida, ventana y políticas de navegación
  backend-controller.mjs backend sidecar, puerto, token, logs y cierre
  preload.mjs             bridge IPC permitido para el renderer
  ipc.mjs                 diálogos, menús contextuales, Finder y drag-out
  menu.mjs                barra de menús y estado nativo de reproducción
src/
  context/PlayerContext.jsx motor multimedia y estado de sesión
  components/             pantallas y controles de interfaz
  hooks/useMediaImport.js importación compartida
  lib/runtime.js          configuración dinámica de desktop o navegador
  lib/api.js              cliente HTTP autenticado
scripts/
  build-backend.mjs       build PyInstaller del sidecar
  stage-backend.mjs       staging del sidecar completo
  stage-native-tools.mjs  staging de FFmpeg/FFprobe y licencias
  generate-icons.mjs      icono ICNS y fondo del DMG
  build-mac-app.mjs       ensamble del bundle macOS sin instalador
  test-packaged-processing.mjs inferencia real con el bundle ensamblado
  package-prepackaged.mjs DMG/ZIP desde el bundle ya validado
build/                    iconos, entitlements y recursos del instalador
```

## Contrato del renderer

El renderer no importa APIs de Node. `preload.mjs`, con `contextIsolation`,
expone `window.ytuDesktop` y sólo permite:

- leer la configuración efímera del runtime;
- abrir selectores nativos;
- resolver rutas de archivos soltados;
- revelar elementos o carpetas conocidas en Finder;
- solicitar menús contextuales;
- iniciar drag-out desde la biblioteca;
- publicar al menú nativo el estado de reproducción y configuración;
- suscribirse a acciones de menú, archivos abiertos y estado del backend.

Las llamadas de dominio pasan por `src/lib/api.js`. No introduzcas puertos,
tokens ni URLs locales en componentes: usa `api`, `apiUrl` o `resourceUrl`.
`resourceUrl` añade el token a contenido multimedia porque los elementos
`audio`/`video` no permiten configurar el encabezado HTTP del cliente Axios.

## Calidad

```bash
npm run lint
npm run build
npm run test:backend
npm run test:e2e
npm run test:headless
npm run test:packaged
npm run test:packaged:processing
npm run test:artifacts
```

`test:headless` es la barrera completa y se ejecuta automáticamente antes de
todos los comandos `desktop:build` y `desktop:dist`. `test:packaged` se ejecuta
después de ensamblar el `.app`: valida su arquitectura y recursos, ejecuta los
sidecars y arranca el renderer de producción con datos temporales.
`test:packaged:processing` genera un WAV corto con el FFmpeg incluido, ejecuta
el modelo BS-RoFormer incluido mediante el sidecar y exige dos stems WAV
válidos. El DMG y el ZIP sólo se crean después de que ambos smoke tests pasan.

El smoke de procesamiento acepta `YTU_PACKAGED_APP` para elegir el bundle,
`YTU_PROCESSING_SMOKE_MODEL` para elegir un checkpoint BS-RoFormer,
`YTU_PROCESSING_SMOKE_TIMEOUT_MS` para ampliar el timeout y
`YTU_KEEP_PROCESSING_SMOKE_DATA=1` para conservar audio y logs temporales.

Las pruebas E2E arrancan Electron sin mostrar la ventana y deben usar datos,
biblioteca y perfil temporales. No deben depender de una cuenta, de la red ni de
la biblioteca real del usuario.

## Build de escritorio

```bash
npm run desktop:build
npm run desktop:dist
npm run desktop:dist:unsigned
```

- `desktop:build` genera una aplicación macOS desempaquetada para inspección.
- `desktop:dist` genera DMG y ZIP para la arquitectura nativa del proceso de
  build y permite firma/notarización.
- `desktop:dist:unsigned` desactiva el descubrimiento automático de identidad de
  firma para pruebas locales.

Los artefactos se escriben en `release/`; `.electron-backend/` y
`.electron-bin/` son directorios de staging generados y no deben editarse.
Consulta el README raíz para las variables de firma y notarización.

## Reglas de contribución del frontend

- Mantén la interfaz básica enfocada; las capacidades especializadas van detrás
  de `show_advanced_tools` o `developer_mode`.
- Las acciones disponibles tanto en UI como en la barra de macOS deben compartir
  una única operación del renderer.
- Todo IPC nuevo debe validar su payload y reducirse a una capacidad concreta.
- Nunca habilites `nodeIntegration`, navegación arbitraria o permisos genéricos.
- Conserva accesibilidad por teclado, estados vacíos y errores comprensibles.
