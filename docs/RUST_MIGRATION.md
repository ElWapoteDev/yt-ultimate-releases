# Estado de la migración a Rust

La versión `5.0.0` sustituye Electron, React y el servicio Python por una aplicación de escritorio cuya interfaz y lógica propia están escritas en Rust. El alcance es **macOS y Windows**; Android e iPhone están aplazados. Es una vista previa funcional, sin declaración de paridad total, equivalencia visual exacta ni preparación para producción.

## Arquitectura

| Capa | Implementación nueva |
| --- | --- |
| Ventana e interfaz | Dioxus Desktop, componentes RSX y señales Rust; Tao/Muda para escritorio |
| Apariencia | CSS original incluido en el binario, adaptaciones y SVG desde Rust |
| Biblioteca y datos | `yt-core`, Rusqlite y SQLite incluido, en el mismo proceso |
| Audio y YouTube | `yt-media`, Rodio/Symphonia, libopus, InnerTube y transporte HTTP |
| Conversión y variaciones | `yt-audio`, LAME, FLAC, SoundTouch y Chromaprint |
| Separación y CALIDAD | BS-RoFormer mediante Rust/LibTorch; DSP Rust y SoXR en `yt-quality` |
| Extensiones y versiones | `yt-plugins` con Wasmi; `yt-updater` con validación de publicación e integridad |
| Distribución | `xtask` Rust y cargo-packager |

La nueva aplicación no arranca Python, Node.js, FFmpeg ni el backend HTTP anterior. Dioxus utiliza WKWebView en Mac y WebView2 en Windows, con un puente JavaScript interno para actualizar el DOM. Conserva CSS y enlaza dependencias C/C++, entre ellas SQLite, QuickJS, libopus, LAME, SoundTouch, SoXR y LibTorch según la edición. «Reescrita en Rust» describe el código propio; no significa que el motor visual y todas sus dependencias sean exclusivamente Rust.

## Funciones y límites actuales

| Función | Implementación comprobable | Límite restante |
| --- | --- | --- |
| Inicio, navegación y aspecto | Inicio, Buscar, Canciones, Playlists, Stems, Descargas, Ajustes y reproductor en Rust, reutilizando estilos originales | Falta comparar todos los estados y pantallas; no se declara equivalencia píxel a píxel |
| Biblioteca | Búsqueda, filtros audio/vídeo, favoritos, lista/cuadrícula, copia o referencia, importación de carpetas y deduplicación | Bibliotecas grandes y archivos con metadatos inusuales necesitan más pruebas de interfaz |
| Escaneo y retirada | Escaneo opcional al inicio; retirar elimina el registro y conserva el archivo; una exclusión persistida evita su reimportación automática | Recuperarlo requiere importación explícita; los archivos externos movidos no se localizan automáticamente |
| M3U/M3U8 | Entradas locales relativas/absolutas; crea o combina una playlist, conserva orden y evita miembros duplicados; coincide por nombre con SQLite NOCASE | Las URL remotas se notifican como no importadas; no se descargan implícitamente |
| Metadatos y carátulas | Lofty, datos anteriores, carátulas locales validadas, sidecars y URL externas | Extracción de carátulas incrustadas y conservación completa de etiquetas/carátulas en exportaciones pendientes; algunos vídeos usan nombre y duración desconocida |
| Playlists locales | Crear, renombrar, eliminar, añadir/quitar y reordenar pistas | Arrastrar para reordenar y exportación completa de playlists pendientes |
| Reproducción de audio | Play/pausa, seek, volumen, velocidad, anterior/siguiente, cola editable, aleatorio, repetición y temporizador | Cambiar velocidad también cambia el tono; no se afirma equivalencia de cada comportamiento histórico |
| Historial y restauración | Pista actual, posición, cola, volumen, velocidad, aleatorio y repetición persistentes; restaura en pausa y filtra archivos ausentes | La validación automatizada no sustituye probar todos los cierres y dispositivos de audio |
| Buscar y reproducir YouTube | Búsqueda, metadatos y resolución reales; descarga completa antes de reproducir | Sin streaming previo a completar la descarga; algunos vídeos, regiones o restricciones del servicio pueden fallar |
| Descargas MP3 | Descarga fuente, conversión a 128/192/256/320 kbps, progreso, cancelación, concurrencia de 1–8 y publicación sin sobrescritura | Sin reanudación de parciales entre ejecuciones; conservación completa de metadatos exportados pendiente |
| Playlists públicas YouTube | Consulta con continuaciones, hasta 500 entradas reproducibles; informa entradas no disponibles y truncamiento. Descarga por lotes y conserva el orden de los elementos conocidos al completar | Máximo de 20 páginas y 120 segundos por consulta; no se garantiza acceso a listas privadas ni listas completas por encima del límite |
| Reintentos y trabajos | Reintentos automáticos acotados en errores de transporte, reintento manual, trabajos SQLite e interrupción reconocida al reiniciar | Subtítulos, plantillas y toda la selección histórica de calidad no están portados; conservar un ajuste no implica aplicarlo |
| Vídeo descargado/local | MP4 progresivo con audio y vídeo juntos; panel con controles del WebView, protocolo local restringido y cierre por temporizador | Sin muxing DASH; resolución limitada al stream progresivo. Códecs del sistema, posición de vídeo no persistida y controles/PiP sin equivalencia completa |
| Conversión local | MP3 con LAME, WAV PCM24 y FLAC24; mono/estéreo, cancelación y destino sin sobrescritura | FLAC limita PCM de entrada a 512 MiB, además de memoria del stream codificado; no convierte todos los canales/contenedores |
| Variaciones y ardilla | Tono y tempo con SoundTouch, EQ Rust, normalización por ganancia constante y exportación FLAC24 | Algoritmos nuevos no son bit a bit iguales a FFmpeg histórico; comparación local no equivale a probar servicios de reconocimiento |
| Separación vocal | Edición completa `native-stems`: modelo BS-RoFormer exportado a TorchScript, inferencia CPU real con ventanas solapadas, voces e instrumental | Salida WAV estéreo a 44,1 kHz; no incluye todos los modelos ni opciones anteriores |
| CALIDAD | Preparación SoXR HQ, separación, EQ instrumental, mezcla con voz preservada, remapeo compartido, estimación de pico y FLAC24 verificado | El estimador de pico no es una certificación IEC; no se promete resultado neural idéntico ni evasión de identificación |
| Huellas | Hash del PCM decodificado y Chromaprint 2; comparación alineada y por desplazamiento, con reporte de muestras cambiadas | La huella usa los primeros 120 segundos; `recognition_tested=false`. No consulta servicios de reconocimiento |
| Menús, archivos y escritorio | Menús Muda, atajos, selectores, apertura de carpetas, argumentos/eventos de archivos, asociaciones y drop de archivos | Falta validar todos los flujos instalados en ambos sistemas, drag-out, recientes, Jump List y todos los contextuales |
| Ajustes | Bitrate MP3, copia/referencia, escaneo, herramientas avanzadas, plugins y actualizaciones; conserva preferencias históricas | La pantalla no expone todas las opciones anteriores; guardar ajustes preserva los registros runtime actuales |
| Plugins | Wasmi, API v2 de comandos, activación explícita, configuración validada y ejemplo Rust/Wasm real | Sin filesystem/red, eventos de fondo, inyección de UI o mutación de biblioteca. Python API v1 se conserva sin ejecutarlo |
| Actualizaciones | Consulta el repositorio anterior, verifica versión/arquitectura/edición y SHA-256, permite descargar y después abrir el instalador | Acciones manuales; no instalación al cerrar, publicación automática ni actualización real de extremo a extremo ya ensayada |

Las tareas de descarga y exportación sobreviven al cierre de su menú o cambio de resultados. El procesamiento utiliza temporales fuera de la biblioteca; la publicación de stems distingue cada resultado aunque se trunque un título largo. La cancelación de FLAC se comprueba entre bloques y los archivos parciales no se publican.

## Compatibilidad de datos y extensiones

`AppCore::open_with_library` admite una base vacía o el esquema v4 anterior. Antes de ampliar por primera vez un perfil v4 crea `pre-rust-<id>.sqlite3` con la API de backup SQLite, incluidos datos confirmados en WAL. Conserva IDs, playlists, ajustes desconocidos y datos de plugins; mantiene el esquema histórico v4 y registra aparte el esquema Rust 2. La actualización Rust 1→2 añade exclusiones de escaneo sin reemplazar tablas históricas. Esquemas anteriores no admitidos, futuros o bases pobladas desconocidas se rechazan antes de migrar.

El perfil predeterminado es `~/Library/Application Support/YT Ultimate` en Mac y `%APPDATA%/YT Ultimate` en Windows. La raíz de biblioteca conserva `Downloads/YT_Ultimate` si existe; en caso contrario usa `Music/YT Ultimate`. Las instalaciones personalizadas pueden indicar `YTU_DATA_DIR` y `YTU_LIBRARY_DIR`. No se deben ejecutar ambas versiones simultáneamente sobre el mismo perfil. Las pruebas usan perfiles temporales; no afirman haber validado toda la biblioteca personal del usuario ni todas las acciones de reversión.

Los plugins nuevos se desactivan inicialmente. Activarlos fija el SHA-256 de manifiesto y módulo; modificar el código exige activación nueva. Wasmi limita instrucciones, memoria y tamaños de entrada/salida, sin imports WASI ni permisos ambientales. Se conserva la configuración Python existente como incompatible. El contrato está en [Plugin API v2](../crates/yt-plugins/README.md).

Las actualizaciones mantienen `ElWapoteDev/yt-ultimate-releases`. Requieren un `digest` SHA-256 de GitHub o `SHA256SUMS` con nombre exacto; si existen ambos deben coincidir. Nunca intercambian silenciosamente base y completa. Se vuelve a verificar el archivo antes de abrirlo. SHA-256 prueba integridad respecto a la publicación, no reemplaza la firma del editor. Véanse [Actualizador](../crates/yt-updater/README.md) y [Empaquetado](RUST_PACKAGING.md).

## Evidencia de verificación

La última ejecución completa aprobó **85 de 85 pruebas**: 1 de `xtask`, 13 de la app, 7 de audio, 16 de core, 14 unitarias de media y 6 de transporte HTTP, 9 de plugins, 10 de CALIDAD y 9 del actualizador. Incluye SQLite real temporal, migración y backup, reimportación, acciones Dioxus, códecs reales, cancelación, límites Wasm y validación de descargas. Las pruebas heredadas Electron/Python permanecen disponibles, pero no prueban el ejecutable Rust.

| Prueba observada en esta Mac | Resultado |
| --- | --- |
| Workspace completo | Formato y Clippy de todos los targets con advertencias como errores aprobados; suite completa ejecutada con funciones de separación habilitadas |
| YouTube real | Búsqueda, metadatos, resolución y descarga AAC completa de un vídeo público; 10.271.496 bytes y decodificación verificados. La conversión MP3 tiene pruebas propias de códec |
| Playlists reales | MIT: 35/35 entradas y límite de 3 correctamente marcado como parcial. Lista de subidas MIT con 8.161 reportadas: 105 recuperadas mediante continuación real y `truncated=true` |
| Audio Mac | Reproducción, pausa, seek, volumen, velocidad y temporizador; decodificación y seek de Ogg Opus sintético |
| Modelo neuronal | Inferencia real de 1 y 9 segundos, ventanas solapadas, frames exactos, salida finita, reconstrucción y cancelación |
| CALIDAD integral | Preparación → BS-RoFormer → DSP → FLAC24 → auditoría en 8,47 segundos para 44.237 frames de entrada. Salida de 43.526 frames; error PCM máximo 8,94e−8, inferior a 2⁻²³; pico estimado −1,09999997 dBTP |
| Paquete Mac completo | Arranque sin rutas externas de LibTorch; importación y separación desde la UI de un audio de 15 segundos, con original/voces/instrumental registrados |
| CALIDAD desde la UI | Procesamiento de un audio de 15 segundos completado al 100%; FLAC de aproximadamente 0,8 MB incorporado a la biblioteca |
| Descarga MP3 desde la UI | Big Buck Bunny, aproximadamente 635 segundos: descarga y conversión completadas al 100%; el trabajo sobrevivió a otra búsqueda que desmontó la fila original |
| Exportación, plugins y versiones desde la UI | Exportación FLAC desde menú mediante diálogo nativo y archivo guardado; instalación/activación del ejemplo Wasm y comando Saludar con resultado «Hola, mundo»; consulta al repositorio real de versiones con respuesta de versión actual |
| Interfaz Mac | Biblioteca, importación, play/pausa, creación/edición de playlists, pistas y búsqueda probadas visualmente; revisión visual completa pendiente |

La prueba CALIDAD procesa aproximadamente un segundo de audio sintético; los 8,47 segundos corresponden al tiempo total de ejecución, no a la duración de la canción. Las métricas y el contrato del DSP están en [yt-quality](../crates/yt-quality/README.md). Los ensayos en servicios reales documentan ejemplos concretos, no garantía frente a cambios futuros de YouTube.

La comprobación final del paquete Mac completo terminó cerrándolo con Cmd+Q y verificando que ya no estuviera ejecutándose. La consulta real de versiones no incluyó descargar ni instalar una actualización publicada nueva.

## Plataformas y distribución

| Destino | Estado observado y límites |
| --- | --- |
| Mac ARM64 base | Compilación y empaquetado intermedios comprobados; macOS 12 mínimo en configuración. Los paquetes base anteriores no son entregables finales |
| Mac ARM64 completo | DMG final `YT Ultimate_5.0.0_aarch64_full.dmg`, 316.630.302 bytes. SHA256SUMS, integridad DMG, firma ad-hoc, arquitectura y dependencias comprobados; arranque final aislado de 10 segundos sin variables externas y cierre correcto. macOS 14 mínimo |
| Windows x64 base | Release MSVC y NSIS intermedios generados desde Mac, con PE32+ e imports comprobados. Los paquetes base anteriores no son entregables finales |
| Windows x64 completo | NSIS final `yt-ultimate_5.0.0_x64_full-setup.exe`, 309.303.263 bytes. SHA256SUMS, integridad del archivo, contenido, PE x64 y dependencias DLL/CRT comprobados. Incluye LibTorch, SoundTouch, SoXR, modelo y bootstrapper Evergreen oficial de WebView2 |
| Windows en ejecución | Ni aplicación ni instalador ejecutados en Windows real; pruebas de arranque, instalación, audio, WebView y separación pendientes |
| Mac Intel | Definido en CI; sin build ni arranque observados todavía |
| MSI y firmas | Configuración disponible; MSI, firma de distribución y notarización pendientes de ejecución y certificados reales |
| Android/iPhone | Fuera de esta entrega; sin objetivos de compilación ni instaladores móviles |

La entrega final de esta sesión incluye solamente **Full para Mac ARM64 y Windows x64**, en sus directorios `full/`. Los paquetes base anteriores se trasladaron a `target/intermediate-packages/<destino>/5.0.0-base-pre-final/`, con `NO_DISTRIBUIR.md`, y no deben distribuirse como la entrega actual. Base y completa tienen nombres distintos para futuras compilaciones y actualizaciones. Los paquetes locales no acreditan firma de distribución, notarización ni actualización desde Electron. No se ha publicado un release ni actualizado automáticamente ninguna instalación. [RUST_PACKAGING.md](RUST_PACKAGING.md) mantiene comandos, nombres, bibliotecas y verificaciones de artefactos.

El NSIS ejecuta el bootstrapper de WebView2 solo cuando el runtime no está instalado, comprueba de nuevo su registro antes de copiar la aplicación y muestra un error si no pudo instalarlo. Ese caso necesita Internet; el modelo y las bibliotecas nativas de la edición completa ya vienen incluidos. Esta lógica de instalación todavía requiere prueba en Windows real.

La auditoría del NSIS final verificó sus 32 entradas, ausencia de Electron/Node/Python y coincidencia de hashes del ejecutable, modelo y bootstrapper extraídos. El DMG y el NSIS finales tienen sus respectivos `SHA256SUMS` junto al instalador.

## Código anterior y trabajo pendiente

Se conservan `frontend/`, `backend/`, sus scripts, pruebas y paquetes anteriores. [LEGACY_ELECTRON.md](LEGACY_ELECTRON.md) conserva íntegro el README original. No había repositorio Git en esta carpeta al iniciar el trabajo.

Para cerrar la equivalencia completa faltan los límites indicados en la matriz, la comparación de pantallas y flujos, validación con datos existentes y ejecución de paquetes instalados en Windows y Mac Intel. La firma y la actualización publicada se deben probar cuando se habilite esa distribución. Los resultados actuales demuestran la reescritura funcional y sus pruebas observadas, no esos pasos todavía pendientes.
