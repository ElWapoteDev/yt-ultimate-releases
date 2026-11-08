# YT Ultimate — escritorio Rust

YT Ultimate `5.0.0` es una reescritura para **macOS y Windows** que conserva la base visual de la aplicación anterior. Los componentes de interfaz, el estado y los servicios de la aplicación están escritos en Rust. Android e iPhone quedan aplazados. Es una vista previa funcional: todavía faltan equivalencia visual completa, algunas funciones históricas y validación del instalador en Windows real.

La ejecución nueva no utiliza Electron, React, Node.js ni el servicio Python anterior. Dioxus representa la interfaz con WKWebView en Mac y WebView2 en Windows, reutiliza el CSS original e incorpora su propio puente JavaScript. También se enlazan bibliotecas nativas: SQLite, QuickJS, libopus, LAME y SoundTouch; la edición completa añade SoXR y LibTorch. La lógica propia es Rust, pero el WebView y todas esas dependencias no están implementados exclusivamente en Rust.

## Funciones disponibles

- Biblioteca SQLite, importación por copia o referencia, carpetas, M3U/M3U8, favoritos y playlists ordenadas. Quitar una canción conserva su archivo y evita que el escaneo la añada de nuevo; una importación explícita permite recuperarla.
- Audio local con play/pausa, seek, volumen, velocidad, cola, aleatorio, repetición, historial, restauración en pausa y temporizador. Vídeo local con controles del WebView.
- Búsqueda y playlists públicas de YouTube, descargas MP3 a 128/192/256/320 kbps, vídeo progresivo, concurrencia limitada, cancelación, reintentos e historial persistente. La reproducción desde Buscar empieza al terminar la descarga.
- Exportación MP3, WAV de 24 bits y FLAC de 24 bits; variaciones de audio, versión ardilla y comparación local de muestras y huellas Chromaprint.
- Edición completa `native-stems`: separación vocal con BS-RoFormer/LibTorch y procesamiento CALIDAD con remuestreo SoXR, EQ, mezcla, ajuste de pico y auditoría del FLAC resultante.
- Plugins Rust/WebAssembly API v2 con activación explícita y ejecución limitada. Los plugins Python anteriores se conservan y se muestran como incompatibles.
- Comprobación de nuevas versiones, descarga verificada por SHA-256 y apertura del instalador mediante acciones separadas del usuario. Conserva la edición base o completa.

No se afirma paridad total con Electron 4.5.1. Siguen pendientes el muxing de pistas DASH, streaming antes de terminar la descarga, equivalencia de controles de vídeo/PiP, extracción de carátulas incrustadas, conservación completa de etiquetas al exportar y algunas integraciones de escritorio y plugins. [Estado de la migración](docs/RUST_MIGRATION.md) detalla implementación, límites y pruebas.

La última suite completa aprobó 85 pruebas y Clippy sin advertencias. En el paquete Mac completo se probaron desde la interfaz exportación FLAC, plugins, consulta de versiones, CALIDAD y descarga/conversión MP3, además de los flujos básicos de biblioteca y reproducción.

## Ejecutar desde el código

Se necesita Rust estable 1.89 o posterior, CMake y un compilador C/C++. En Mac: Xcode Command Line Tools. En Windows: toolchain MSVC, Visual Studio C++ Build Tools, Windows SDK y [WebView2 Runtime](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution).

```sh
cargo run --locked -p yt-app --bin yt-ultimate
```

`start_app.command` y `start_app.ps1` arrancan esta versión Rust. La ruta nueva no necesita npm ni un intérprete Python. La edición completa requiere LibTorch y el modelo previamente exportado; véase [Empaquetado Rust](docs/RUST_PACKAGING.md).

Para usar un perfil de prueba separado:

```sh
YTU_DATA_DIR=/tmp/ytu-rust-profile YTU_LIBRARY_DIR=/tmp/ytu-rust-library \
  cargo run --locked -p yt-app --bin yt-ultimate
```

En PowerShell:

```powershell
$env:YTU_DATA_DIR = Join-Path $env:TEMP 'ytu-rust-profile'
$env:YTU_LIBRARY_DIR = Join-Path $env:TEMP 'ytu-rust-library'
cargo run --locked -p yt-app --bin yt-ultimate
```

## Datos existentes

El perfil predeterminado coincide con Electron: `~/Library/Application Support/YT Ultimate` en Mac y `%APPDATA%/YT Ultimate` en Windows. La biblioteca usa `Downloads/YT_Ultimate` si existe; en caso contrario, `Music/YT Ultimate`. `YTU_DATA_DIR` y `YTU_LIBRARY_DIR` permiten seleccionar otras carpetas.

Antes de ampliar una base anterior compatible con esquema v4, se crea `pre-rust-<id>.sqlite3` mediante la API de backup SQLite, incluidos los datos confirmados del WAL. Se conservan identificadores, playlists, ajustes desconocidos y registros de plugins. El esquema Rust 2 añade exclusiones de escaneo; guardar preferencias no sobrescribe la reproducción actual. Los esquemas antiguos no compatibles o futuros se rechazan antes de migrar. Cerrar la aplicación anterior antes de usar el mismo perfil en Rust.

## Verificar y empaquetar

```sh
cargo run --locked -p xtask -- check
cargo install cargo-packager --version 0.11.8 --locked --root target/packager-tools
cargo run --locked -p xtask -- package
```

`check` ejecuta formato, pruebas y Clippy. `package` genera `.app`/`.dmg` en Mac o instaladores Windows según el destino. Los resultados quedan en `target/packages/<destino>/5.0.0/`; la edición completa usa `full/` y nombres de archivo distintos. Cada generación incluye `SHA256SUMS`.

Los entregables de esta sesión son únicamente los paquetes **Full** de Mac ARM64 y Windows x64. Los paquetes base generados antes se trasladaron a `target/intermediate-packages/`, identificados con `NO_DISTRIBUIR.md`; no representan la entrega final ni deben publicarse como tal. El modo base sigue disponible para compilaciones futuras.

Los instaladores finales verificados son `YT Ultimate_5.0.0_aarch64_full.dmg` y `yt-ultimate_5.0.0_x64_full-setup.exe`, dentro de `target/packages/<destino>/5.0.0/full/`, cada uno con su manifiesto SHA-256.

Mac Full requiere macOS 14; la configuración base declara macOS 12. El NSIS Full de Windows incluye el instalador oficial Evergreen de WebView2: si falta el runtime, lo instala antes de continuar, con conexión a Internet. El modelo y las demás bibliotecas nativas se incluyen en el paquete.

Intel está definido en CI y pendiente de ejecución. Compilar Windows desde Mac no sustituye probar su instalación y ejecución en Windows. Los paquetes locales no tienen firma de distribución ni notarización. No se han publicado releases ni actualizado instalaciones anteriores automáticamente. [Empaquetado Rust](docs/RUST_PACKAGING.md) mantiene los comandos, requisitos y verificaciones de los artefactos.

## Código y versión anterior

```text
crates/
  yt-app/       Interfaz Dioxus, estado y escritorio
  yt-core/      Biblioteca, playlists, ajustes e historial SQLite
  yt-media/     Reproducción, YouTube, descargas y separación neuronal
  yt-audio/     Conversión, variaciones y huellas de audio
  yt-quality/   DSP y auditorías de CALIDAD
  yt-plugins/   Intérprete Wasm y Plugin API v2
  yt-updater/   Actualizaciones verificadas por edición
  xtask/        Verificación y empaquetado
packaging/      Iconos, asociaciones y configuración de instaladores
docs/          Migración y documentación técnica
frontend/      Electron/React anterior conservado
backend/       Python anterior conservado
```

Las fuentes, pruebas y paquetes anteriores se conservan. El README original está íntegro en [Documentación de Electron 4.5.1](docs/LEGACY_ELECTRON.md); sus comandos y los documentos históricos describen esa versión. Esta carpeta no tenía repositorio Git al iniciar la migración; conservar fuentes no sustituye un historial de versiones ni una copia de seguridad.
