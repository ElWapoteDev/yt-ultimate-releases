# YT Ultimate — escritorio Rust

YT Ultimate 5.0.1 es la versión de escritorio para **macOS y Windows**. Conserva los estilos de Electron y reescribe en Rust sus componentes, estado y servicios. Android e iPhone quedan aplazados.

[Descargas de 5.0.1](https://github.com/ElWapoteDev/yt-ultimate-releases/releases/tag/v5.0.1). La publicación incluye los instaladores Full de Mac Apple Silicon, Mac Intel y Windows x64, los hashes SHA-256 y las fuentes correspondientes.

La ejecución nueva no utiliza Electron, React, Node.js ni el servicio Python anterior. Dioxus utiliza WKWebView en Mac y WebView2 en Windows, reutiliza el CSS original e incorpora su puente JavaScript. Un adaptador pequeño conecta el elemento de vídeo del WebView con el estado Rust, incluidos PiP y pantalla completa. También se enlazan bibliotecas nativas como SQLite, QuickJS, libopus, LAME y SoundTouch; la edición completa añade SoXR y LibTorch. «Rust» describe la implementación de la aplicación, no que el WebView, ese adaptador y todas las dependencias estén escritos exclusivamente en Rust.

## Funciones del código actual

- Biblioteca SQLite, importación por copia o referencia, carpetas, favoritos y playlists ordenadas. Importación M3U/M3U8 y exportación M3U8 con rutas reproducibles; quitar una pista conserva el archivo y evita que el escaneo la añada de nuevo.
- Carátulas incrustadas extraídas a caché validada. Exportación MP3, WAV24 y FLAC24 con etiquetas y carátulas, incluyendo las nuevas descargas y resultados procesados.
- Audio local y streaming de YouTube antes de completar la descarga; seek, volumen, velocidad conservando tono, cola, aleatorio, repetición, historial, restauración en pausa y temporizador.
- Búsqueda y playlists públicas de YouTube; descargas MP3 a 128/192/256/320 kbps y vídeo MP4 con selección de resolución, unión DASH sin recodificar, pausas, reanudación tras reiniciar, reintentos y concurrencia limitada.
- Subtítulos WebVTT junto al vídeo, plantillas de nombre y cookies Netscape elegidas explícitamente. El acceso privado depende del servicio y no se extraen cookies del navegador automáticamente.
- Biblioteca con filtros/lista/cuadrícula e información de audio; playlists con portadas, aleatorio, reordenación y borrado confirmado; Stems con selector de fuente, formato, progreso y guardado de resultados; diálogo ardilla con presets y controles de tono, velocidad y brillo.
- Edición completa `native-stems`: BS-RoFormer/LibTorch para voz e instrumental; CALIDAD con SoXR, EQ, mezcla, ajuste de pico y auditoría del FLAC. Variaciones y huellas Chromaprint funcionan también en la edición base.
- Plugins Rust/WebAssembly API v2 con permisos explícitos, comandos, eventos, estado persistente y acceso acotado a biblioteca. Incluye ejemplos y creación de proyectos Rust/Wasm. Los plugins Python anteriores se conservan sin ejecutarse.
- Actualizador con comprobación de versión, arquitectura, edición y SHA-256, comprobación y descarga automática configurables, aviso para reiniciar o posponer y opción explícita de instalar al salir. El helper espera el cierre y vuelve a validar el instalador.

Las funciones añadidas después de 5.0.0 se detallan en [Estado de la migración](docs/RUST_MIGRATION.md), junto con sus pruebas y límites. Las pruebas del código nuevo incluyen metadatos, covers, M3U8, plugins, streaming, reanudación, muxing y controles de interfaz. La suite Full verifica la aplicación, sus bibliotecas y el empaquetado. Los informes de instaladores y las notas del release documentan la validación de cada paquete y su commit exacto.

## Ejecutar desde el código

Se necesita Rust estable 1.89 o posterior, CMake y un compilador C/C++. En Mac: Xcode Command Line Tools. En Windows: toolchain MSVC, Visual Studio C++ Build Tools, Windows SDK y [WebView2 Runtime](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution).

```sh
cargo run --locked -p yt-app --bin yt-ultimate
```

`start_app.command` y `start_app.ps1` arrancan esta versión Rust. La ruta nueva no necesita npm ni un intérprete Python. La edición completa requiere LibTorch y el modelo preparado; véase [Empaquetado Rust](docs/RUST_PACKAGING.md).

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

El perfil coincide con Electron: `~/Library/Application Support/YT Ultimate` en Mac y `%APPDATA%/YT Ultimate` en Windows. La biblioteca usa `Downloads/YT_Ultimate` si existe; en caso contrario, `Music/YT Ultimate`. `YTU_DATA_DIR` y `YTU_LIBRARY_DIR` permiten elegir otras carpetas.

Antes de ampliar una base compatible con esquema v4, se crea `pre-rust-<id>.sqlite3` mediante la API de backup SQLite, incluidos los datos confirmados del WAL. Se conservan identificadores, playlists, preferencias desconocidas y registros de plugins. El esquema Rust 2 añade exclusiones de escaneo; guardar preferencias no sobrescribe la reproducción actual. Los esquemas antiguos incompatibles o futuros se rechazan antes de migrar. Cerrar la aplicación anterior antes de usar el mismo perfil en Rust.

## Verificar y empaquetar

```sh
cargo run --locked -p xtask -- check
cargo install cargo-packager --version 0.11.8 --locked --root target/packager-tools
cargo run --locked -p xtask -- package
```

`check` ejecuta formato, pruebas y Clippy. `package` genera `.app`/`.dmg` en Mac o instaladores Windows según el destino. Los resultados quedan en `target/packages/<destino>/<versión>/`; la edición completa usa `full/` y nombres distintos. Cada generación incluye `SHA256SUMS`.

Los instaladores de **Full 5.0.1** son un NSIS para Windows x64 y dos DMG para Mac Apple Silicon e Intel. Full requiere macOS 14 o posterior. La configuración base declara macOS 12. El NSIS incluye el bootstrapper oficial de WebView2 y lo instala, con conexión a Internet, únicamente si falta. El modelo y las demás dependencias nativas ya están incluidos.

El instalador Windows publicado pasó instalación silenciosa, arranque con ventana/WebView2, asociaciones y desinstalación en Windows Server 2022 de CI. Eso no acredita pruebas manuales de audio ni todos los flujos de Windows 10/11. Mac ARM64 pasó arranque y flujos de interfaz. Los checkpoints 5.0.1 de Windows e Intel Full ya aprobaron instalación e inferencia con las dependencias empaquetadas; Windows también aprobó la actualización real desde Electron 4.4.1 y su helper de reinstalación. Las notas del release enlazan la validación correspondiente a los instaladores finales. Los paquetes no tienen firma de distribución ni notarización. Las compilaciones base intermedias están identificadas en `target/intermediate-packages/` y no son los instaladores publicados.

## Código y versión anterior

```text
crates/
  yt-app/       Interfaz Dioxus, estado y escritorio
  yt-core/      Biblioteca, playlists, ajustes e historial SQLite
  yt-media/     Reproducción, YouTube, transporte, muxing y separación
  yt-audio/     Conversión, metadatos, variaciones y huellas de audio
  yt-quality/   DSP y auditorías de CALIDAD
  yt-plugins/   Intérprete Wasm, permisos y Plugin API v2
  yt-updater/   Actualizaciones verificadas por edición
  xtask/        Verificación y empaquetado
packaging/      Iconos, asociaciones, instaladores y ensayos CI
docs/          Migración y documentación técnica
frontend/      Electron/React anterior conservado
backend/       Python anterior conservado
```

Las fuentes, pruebas y paquetes anteriores se conservan. El README original está íntegro en [Documentación de Electron 4.5.1](docs/LEGACY_ELECTRON.md); sus comandos y los documentos históricos describen esa versión, no el ejecutable Rust.
