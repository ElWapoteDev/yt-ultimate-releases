# Estado de la migración a Rust

El alcance de **5.0.1** es macOS y Windows. Android e iPhone están aplazados. Las notas del [release 5.0.1](https://github.com/ElWapoteDev/yt-ultimate-releases/releases/tag/v5.0.1) identifican los instaladores y la validación de su commit. La [prerelease 5.0.0](https://github.com/ElWapoteDev/yt-ultimate-releases/releases/tag/v5.0.0) conserva sus fuentes e instaladores.

## Arquitectura

| Capa | Implementación |
| --- | --- |
| Interfaz y estado | Dioxus Desktop, componentes RSX y señales Rust; CSS original y SVG desde Rust |
| Escritorio | Tao/Muda, selectores nativos, asociaciones, recientes y arrastre de archivos |
| Datos | `yt-core`, Rusqlite y SQLite incluido, dentro del mismo proceso |
| Audio y YouTube | `yt-media`, Rodio/Symphonia, libopus, SoundTouch, InnerTube, HTTP y muxing MP4 Rust |
| Exportación | `yt-audio`, LAME, FDK-AAC, Vorbis/Opus, FLAC por bloques, Lofty, SoundTouch y Chromaprint |
| Separación y CALIDAD | BS-RoFormer con LibTorch; DSP Rust y SoXR en `yt-quality` |
| Plugins y versiones | `yt-plugins` con Wasmi y permisos acotados; `yt-updater` con validación de integridad |
| Distribución | `xtask` Rust, cargo-packager y pruebas de instaladores en CI |

La aplicación no arranca Python, Node.js, FFmpeg ni el backend HTTP anterior. Dioxus utiliza WKWebView/WebView2 y un puente JavaScript para actualizar el DOM. Además, adaptadores JavaScript pequeños controlan vídeo/PiP y primitivas DOM de selección, edición y teclado; validación, estado, persistencia y cola permanecen en Rust. Se enlazan bibliotecas C/C++, incluidas SQLite, QuickJS, libopus, libvorbis, FDK-AAC, LAME, SoundTouch y, según edición, SoXR/LibTorch. «Rust» describe la reescritura de la aplicación, no que todas sus dependencias o ese adaptador sean exclusivamente Rust.

## Funciones del código 5.0.1 y límites

| Área | Implementado | Límite restante |
| --- | --- | --- |
| Pantallas | Inicio, Buscar, Canciones, Playlists, Stems, Descargas, Ajustes y reproductor. Biblioteca con lupa/limpiar, filtros, favoritos, lista/cuadrícula e información; playlists con portadas, aleatorio, opciones, confirmación de borrado, DnD y botones de orden; Stems con fuentes/formato/trabajos y diálogo ardilla | Pruebas de renderizado y acciones no sustituyen comparar todos los estados visuales instalados |
| Biblioteca | Copia/referencia, carpetas, búsqueda, favoritos, deduplicación y escaneo opcional. Retirar conserva el archivo y crea una exclusión; importar explícitamente la levanta | Archivos externos movidos requieren reimportación; no se ha validado toda biblioteca personal posible |
| M3U/M3U8 | Importa rutas locales, combina playlists sin duplicados, conserva orden y compara nombres con SQLite NOCASE. Exporta UTF-8 extended M3U8 con rutas relativas cuando corresponde y absolutas fuera del destino | URL remotas y archivos ausentes/no representables se informan; no se descargan implícitamente |
| Metadatos y carátulas | Extrae covers incrustados a caché validada por contenido; conserva etiquetas y covers al exportar MP3/WAV/FLAC/M4A/AAC/OGG/Opus y procesar audio. Descargas incorporan título, artista, procedencia y cover | El contenido legible depende de Lofty y del formato. Datos no portables se archivan en campos propios; otros reproductores no necesariamente los interpretan. MKV/WebM y MP4 sin audio tienen lectura de duración y etiquetas, con reparación de registros de duración cero sin cambiar títulos editados |
| Audio | Play/pausa, seek, volumen, velocidad 0,25–4× con tono preservado, cola, aleatorio, repetición y temporizador | La salida de audio depende del sistema. Historial de sesión separado, Anterior consecutivo y repetición tardía corregidos y probados; las pruebas manuales Windows amplias siguen siendo un alcance distinto de CI |
| Restauración | Pista, posición, cola local/remota, volumen, velocidad, aleatorio y repetición persistentes; restaura en pausa. Los registros remotos usan URL canónicas, nunca URL firmadas | El historial remoto se restaura y distingue las versiones audio/vídeo de un mismo contenido. La selección se guarda antes de resolver la red. No implica que un recurso remoto siga disponible tras reiniciar |
| Streaming YouTube | Caché por rangos: abre antes de descargar todo, prioriza seek, indica buffering y renueva una URL caducada. Vídeo remoto y local integrado con controles compartidos, posición, PiP/pantalla completa mediante WebView | Si falta un progresivo compatible, prepara un MP4 H.264/AAC temporal con cancelación. Reconexión acotada y seek ligados a la sesión evitan reactivar una pista cancelada. MKV AVC/HEVC+AAC se prepara en MP4 y VP8/VP9+Vorbis/Opus en WebM; conserva paquetes, cuadros B y tiempos variables. Originales intactos y temporales cancelables. PiP, autoplay y códecs dependen del WebView |
| Descargas | MP3 128/192/256/320 kbps, MP4 progresivo/DASH, resolución máxima o límite elegido, concurrencia, reintentos, pausa/reanudar y caché duradera validada por identidad/tamaño/hash | El formato disponible depende de YouTube; no promete superar desafíos de acceso. Cachés incompatibles o corruptas se rechazan |
| DASH | Une vídeo MP4 y audio AAC, incluidos fragmentos, sin FFmpeg ni recodificación; comprueba muestras, configuración y tiempos | Rechaza segmentos/metadatos mayores de 64 MiB, más de 2 millones de muestras por pista o timelines incompatibles. AV1/VP9 siguen dependiendo del decodificador del sistema |
| Playlists remotas | Continuaciones reales; hasta 500 entradas reproducibles, 20 páginas y 120 segundos. Informa truncamiento/no disponibles y mantiene orden del lote al finalizar concurrentemente | No garantiza listas privadas ni recuperar listas que exceden los límites |
| Preferencias de descarga | Plantillas `{title}`, `{artist}`, `{id}`, calidad, metadatos/covers, subtítulos WebVTT, timeout/reintentos y cookies Netscape elegidas por el usuario. El cliente admite proxy HTTP(S) | Extracción explícita de Firefox/Safari y Chromium compatible, sólo al pulsar Conectar; pruebas con perfiles sintéticos. Chromium App-Bound v20 se rechaza sin inyección ni elevación. Sin login interactivo, desafíos PO-token ni grabación en directo; cuenta real no validada |
| Conversión y variantes | MP3, WAV24, FLAC24, M4A, AAC ADTS, OGG Vorbis y Opus; cancelación y publicación sin sobrescritura. SoundTouch/EQ para variaciones y ardilla, con presets y sliders | Mono/estéreo. FLAC se escribe por bloques sin el límite anterior de 512 MiB; probado con una fuente virtual mayor. Más de 192 kHz usa remuestreo temporal para SoundTouch. DSP y hashes PCM tienen algoritmos identificados y no son bit a bit iguales a FFmpeg histórico |
| Stems | Edición completa: BS-RoFormer, autodetección CUDA/MPS con fallback CPU y ventanas solapadas; voz/instrumental exportables en FLAC, MP3 o WAV. Cola y concurrencia de procesos pesados | Modelo preparado requerido, el mismo modelo único ofrecido por la UI anterior. En este Mac el modelo congelado contiene float64 no soportado por MPS y cae a CPU; Windows Full incluye LibTorch CPU |
| CALIDAD y huellas | Preparación SoXR HQ → separación → EQ/mezcla/remapeo → pico → FLAC24 auditado. Hash PCM y Chromaprint con comparación alineada; información local desde la biblioteca | Estimador de pico sin certificación IEC. Huella acústica limitada a 120 segundos; no consulta servicios externos ni promete resultados de reconocimiento |
| Escritorio y ajustes | Menús/atajos, clic derecho nativo Mac y en página Windows, edición de texto, controles multimedia del SO, instancia única por perfil, apertura/drop, asociaciones, recientes, arrastre nativo, logs acotados y preferencias | Integración nativa nueva todavía requiere comprobación visual completa en ambas plataformas. Conservar preferencias históricas no implica reproducir toda opción antigua |
| Plugins | API v2 Wasm, permisos explícitos `library.read`, `library.write`, `state`, `events`; comandos, estado aislado, eventos del host, proyecto Rust y journal avanzado | Python v1 se conserva sin ejecución ni conversión automática. Sin filesystem/red ambientales, tareas guest persistentes ni inyección arbitraria de UI |
| Actualizaciones | Repositorio original, versión/arquitectura/edición exactas, SHA-256, comprobación/descarga automática, aviso reiniciar/posponer y opción explícita de instalar al salir; helper Rust espera el cierre y revalida antes de instalar | DMG aislado con firma ad-hoc, montaje, sustitución atómica y rollback probados. NSIS instalado comprobado: helper espera el proceso, reinstala, reinicia con el mismo perfil y cierra con código nativo 0. El hash no acredita firma del editor |

Las tareas sobreviven al cierre de su menú o cambio de página. Los temporales de procesamiento y descargas quedan fuera de la biblioteca; publicación y copia de etiquetas protegen los originales. Los stems se publican por lote. Pausar conserva parciales reanudables; descartarlos comprueba otros trabajos que comparten la caché, incluidos registros anteriores al límite de 500 mostrado en la interfaz.

## Datos y extensiones anteriores

`AppCore::open_with_library` admite una base vacía o el esquema v4 anterior. Antes de ampliar un perfil v4 crea `pre-rust-<id>.sqlite3` mediante backup SQLite, incluidos datos confirmados en WAL. Conserva IDs, playlists, ajustes desconocidos y datos de plugins; mantiene el esquema histórico v4 y registra aparte el esquema Rust 2. La migración Rust 1→2 añade exclusiones de escaneo. Bases antiguas incompatibles, futuras o pobladas desconocidas se rechazan antes de migrar.

El perfil es `~/Library/Application Support/YT Ultimate` en Mac y `%APPDATA%/YT Ultimate` en Windows. Se conserva `Downloads/YT_Ultimate` si existe; en caso contrario se usa `Music/YT Ultimate`. `YTU_DATA_DIR`/`YTU_LIBRARY_DIR` permiten perfiles aislados. No ejecutar Electron y Rust simultáneamente sobre el mismo perfil. El ensayo instalado Windows Electron 4.4.1 → Rust está aprobado: conserva ruta, IDs, playlists, biblioteca, plugins y backup del perfil; también mantiene los nombres de ejecutable y accesos anteriores.

Los plugins nuevos empiezan desactivados. Activarlos fija el SHA-256 de manifiesto/módulo; cambios de código o permisos exigen activación nueva. Wasmi limita fuel, memoria, llamadas y datos. El acceso concedido modifica registros y playlists, nunca originales ni SQL arbitrario. El journal Rust guarda eventos limitados en estado aislado. Véase [Plugin API v2](../crates/yt-plugins/README.md).

El actualizador mantiene `ElWapoteDev/yt-ultimate-releases`. Requiere digest SHA-256 de GitHub o `SHA256SUMS` con nombre exacto; si existen ambos deben coincidir. Base y Full nunca se intercambian silenciosamente. Revalida antes de abrir el archivo; integridad respecto al release no sustituye firma del editor. Véanse [Actualizador](../crates/yt-updater/README.md) y [Empaquetado](RUST_PACKAGING.md).

## Verificación observada

La entrega publicada 5.0.0 aprobó **85 de 85 pruebas**, formato y Clippy del workspace completo con separación habilitada. El checkpoint integrado 5.0.1 aprobó **219 pruebas**, con 7 explícitamente ignoradas, formato y Clippy del workspace completo con separación habilitada. Incluye las regresiones de cancelación, persistencia, arrastre y recepción de archivos durante importaciones lentas; no se suman ejecuciones parciales.

| Evidencia | Resultado observado |
| --- | --- |
| Core/audio/plugins nuevos | Pruebas con SQLite temporal, tags/covers MP3/WAV/FLAC, M3U8, limpieza, restauración remota y guests Wasm reales aprobadas; no usan el perfil personal |
| Nuevas pantallas | 10 pruebas aprobadas: 4 de procesamiento, 3 de biblioteca/información y 3 de playlists. Incluyen renderizado, clics VirtualDom, confirmación de borrado, covers reales y copia exacta del resultado; Clippy app/core con advertencias como errores aprobado |
| Caché compartida | Regresión core aprobada: encuentra original y reintento aunque queden detrás de 501 trabajos más recientes; tolera JSON legado inválido |
| YouTube y playlists reales | Audio público completo de 10.271.496 bytes y 634,625 s. MIT: 35/35, límite 3 marcado parcial y 105 entradas por continuación de uploads con 8.161 reportadas |
| Streaming | Inicio de decodificación en 167 ms con 262.144 de 10.271.496 bytes; seek a 120 s antes de terminar. Servidor lento local cubierto por regresión |
| Reanudar | Pausa a 1.048.576 bytes, nuevo cliente/resolución, reanudación y decodificación de los 634,625 s completos; pruebas de cola de escritura tras crash, corrupción e identidad incompatible |
| DASH | 4 pruebas del mux y fixture real de aproximadamente 635 s conservan muestras/configuración/tiempos de audio y vídeo. Flujo app descarga → carátula → mux → publicación → índice aprobado: MP4 de 15.135.242 bytes a 144p en 7,53 s |
| Subtítulos y tempo | Consulta real: 36 idiomas y 80 cues ingleses WebVTT. Tono de prueba de 440 Hz conservado a velocidad 2× |
| Modelo y CALIDAD | Inferencia real de 1 y 9 s con ventanas, reconstrucción y cancelación. Cadena CALIDAD completa de 44.237 frames en 8,47 s; error PCM máximo 8,94e−8, menor que 2⁻²³; pico estimado −1,09999997 dBTP |
| UI del paquete Mac 5.0.0 | Importación, play/pausa, playlists, búsqueda, separación y CALIDAD de 15 s, MP3 completo de Big Buck Bunny, exportación FLAC nativa, plugin Saludar y consulta real de versiones; cierre Cmd+Q comprobado |
| UI integrada 5.0.1 | Perfil aislado: menú nativo de Inicio/Biblioteca, cola, play/pausa/seek, Espacio, Cmd+F, cortar/pegar por menú, búsqueda/stream real, ajustes, huella local, Stems de 15 s, exportación idéntica al resultado, fingerprint Intenso/Equilibrado, CALIDAD, Ardilla acelerada, crear/renombrar/exportar playlists aprobados. MKV AVC/HEVC/VFR, WebM y MKV VP8 reproducidos; pausa, seek y PiP comprobados. Apertura desde segunda instancia importa cuatro vídeos en la ventana existente. Originales intactos; comparación visual completa en curso |

Los 8,47 segundos de CALIDAD corresponden al tiempo de cálculo sobre aproximadamente un segundo de audio sintético, no a la duración de una canción. Los ensayos online prueban ejemplos concretos, no inmunidad frente a cambios de YouTube. Los detalles reproducibles están en [yt-media](../crates/yt-media/README.md) y [yt-quality](../crates/yt-quality/README.md).

## Plataformas y releases

| Destino | Estado observado |
| --- | --- |
| Mac ARM64 Full 5.0.0 | DMG publicado de 316.630.302 bytes; SHA256SUMS, integridad, firma ad-hoc, arquitectura/dependencias y arranque aislado comprobados. Mínimo macOS 14 |
| Windows x64 Full 5.0.0 | NSIS publicado de 309.303.263 bytes; integridad, 32 entradas, hashes de ejecutable/modelo/bootstrapper y DLL/CRT comprobados; sin Electron/Node/Python |
| Windows nativo en CI | NSIS publicado instalado en carpeta con espacios sobre Server 2022, ventana visible, proceso WebView2, SQLite aislado, asociaciones, cierre con código 0 y desinstalación aprobados en dos ejecuciones |
| Windows interactivo | Sin ensayo manual completo de audio, vídeo, GPU o separación en Windows 10/11; Server 2022 no acredita esos flujos |
| Edición base CI | Formato, pruebas, Clippy y compilación release aprobados para Mac ARM64, Mac Intel y Windows x64 en una revisión previa; no incluía empaquetado ni convierte los paquetes intermedios en entregables publicados |
| Mac Intel | Checkpoint Full aprobado: LibTorch 2.12 compilado desde fuentes oficiales, 197 pruebas, Clippy, DMG x86_64 y aplicación copiada desde el instalador con Stems/CALIDAD. Todos los Mach-O declaran mínimo 14.0; ejecución real en macOS 15.7.9. La fuente integrada posterior debe volver a empaquetarse |
| Full Windows 5.0.1 y upgrade | Checkpoint instalado aprobado: 192 pruebas, NSIS, WebView2, dos nombres EXE con hash idéntico, instancia única, Stems/CALIDAD, helper de actualización y upgrade real Electron 4.4.1 conservando perfil/ruta/archivos. Fuente posterior aún requiere sus paquetes finales |
| Firmas y MSI | Sin firma de distribución ni notarización; MSI no declarado validado |
| Android/iPhone | Aplazados, sin binarios ni instaladores móviles |

La prueba Windows publicada está registrada en [CI de instalación nativa](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42518474582/job/128846564064) y [su segunda ejecución](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42520151853/job/128851666763). La [compilación base de las tres arquitecturas](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42520151859) también aprobó. El [checkpoint Full Windows](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42540803646), la [prueba del actualizador instalado](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42540803681) y el [checkpoint Full Intel](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42539060978) completaron sus comprobaciones. Cada evidencia corresponde al commit de su ejecución y no se atribuye automáticamente a cambios posteriores.

El NSIS incluye Evergreen WebView2 oficial: lo ejecuta silenciosamente solo cuando falta el runtime, vuelve a comprobar el registro y falla con un mensaje claro si no pudo instalarlo. Ese caso necesita Internet. Modelo y bibliotecas nativas de Full ya están incluidos. No se ha probado aquí una instalación sin red ni una máquina limpia de todas las versiones WebView2.

El release 5.0.0 permanece inmutable; el desarrollo posterior no se atribuye a sus fuentes o binarios. Las bases locales anteriores están en `target/intermediate-packages/<destino>/5.0.0-base-pre-final/` con `NO_DISTRIBUIR.md`. Los nuevos resultados se generan bajo la versión correspondiente en `target/packages/`. [RUST_PACKAGING.md](RUST_PACKAGING.md) mantiene requisitos, nombres y comprobaciones del empaquetado.

## Checkpoint de paridad y trabajo pendiente

Se conservan `frontend/`, `backend/`, scripts, pruebas y paquetes anteriores; [LEGACY_ELECTRON.md](LEGACY_ELECTRON.md) contiene el README original. La conservación no convierte automáticamente extensiones Python ni acredita todos los perfiles antiguos.

Historial, Matroska, upgrade instalado e Intel Full ya tienen implementación y pruebas concretas. Se corrigieron el orden de inicialización AppKit, el rechazo de autoimportación al arrastrar y la recepción continua de archivos durante importaciones largas. Sus comprobaciones de código y regresiones pasan. La prueba comparativa del selector nativo reproduce la misma limitación de Cmd+A/V en Electron y Rust mediante la herramienta de control; escribir y las acciones del menú funcionan. No se considera una regresión demostrada de Rust ni se declara una corrección del teclado físico. El monitor experimental de atajos se retiró. La comparación de Biblioteca y Stems recuperó la captura al elevar la ventana de prueba. Se ajustaron el color de fondo y los iconos de reproducción a partir de las diferencias observadas. Una regresión acciona las siete páginas y verifica el contenido y el estado activo de la navegación. Quedan la comparación visual completa y los flujos manuales Windows no cubiertos por CI. Los tres instaladores deben aprobar los workflows Full del mismo commit; sus informes BUILD-SOURCE.json y las notas del release identifican la fuente y el resultado final. No se declara equivalencia visual exacta sin esa evidencia. Los paquetes sin certificados de distribución se identifican como tales; Android/iPhone siguen fuera del alcance.
