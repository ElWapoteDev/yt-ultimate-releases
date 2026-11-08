# Empaquetado de la aplicación Rust

El ejecutable nuevo es `yt-ultimate`, del paquete `yt-app`. Los componentes y estilos de la interfaz se incluyen en el binario. Los instaladores nuevos no incluyen Electron, Node.js, un servidor Python ni el backend anterior. Los archivos anteriores se conservan como referencia durante la migración; generar un instalador no demuestra paridad funcional con esa versión.

## Plataformas y requisitos

| Destino | Compilación | Instalador | Requisitos del equipo de destino |
| --- | --- | --- | --- |
| macOS Apple Silicon | Mac ARM64, Rust estable, CMake y Xcode Command Line Tools | `.app`, `.dmg` | macOS 12 o posterior; WKWebView del sistema |
| macOS Apple Silicon con separación de voces | Lo anterior, LibTorch 2.12 ARM64 y el modelo exportado | `.app`, `.dmg` en `full/` | macOS 14 o posterior; bibliotecas y modelo incluidos |
| macOS Intel | Mac Intel, Rust estable, CMake y Xcode Command Line Tools | `.app`, `.dmg` | macOS 12 o posterior; WKWebView del sistema |
| Windows x64 | Windows x64, Rust MSVC estable, Visual Studio C++ Build Tools, CMake, LLVM/libclang y Windows SDK | NSIS `.exe`, WiX `.msi` base | Windows 10/11 x64 y Microsoft Edge WebView2 Runtime |
| Windows x64 con separación y CALIDAD | Lo anterior, LibTorch 2.12 para Windows, DLL de Microsoft CRT y el modelo | NSIS `_full-setup.exe` | Windows 10/11 x64 y WebView2; modelo y DLL nativas incluidos |

Android e iOS quedan fuera de este alcance. El flujo `.github/workflows/rust-desktop.yml` define los tres destinos en su propio sistema; también existe una ruta de compilación cruzada de Mac a Windows x64 descrita abajo. No se anuncia compatibilidad con un destino hasta ejecutar su compilación y sus pruebas. `cargo-packager` descarga sus herramientas WiX/NSIS cuando las necesita; su primera ejecución requiere red.

La edición Intel que valida ese flujo es la **base**. El [índice oficial de LibTorch](https://download.pytorch.org/libtorch/cpu/) ofrece binarios macOS x86_64 hasta 2.2.2, mientras esta edición Full requiere la ABI 2.12. PyTorch [retiró las distribuciones oficiales para Mac Intel](https://dev-discuss.pytorch.org/t/pytorch-macos-x86-builds-deprecation-starting-january-2024/1690). No es correcto sustituirlas por 2.2 ni por bibliotecas ARM64: Full Intel necesita primero una compilación compatible de LibTorch desde fuentes y su validación de inferencia. No se anuncia ese instalador como compatible actualmente.

El workflow independiente `.github/workflows/macos-intel-full.yml` intenta cerrar esa limitación sin cambiar la ABI global: compila el source oficial 2.12.0 con hash fijado, C++20, CPU Intel, Accelerate/vecLib, PocketFFT y OpenMP. Python se utiliza únicamente para generar fuentes durante la construcción. Desactiva las aceleraciones GPU/distribuidas/cuantizadas que no usa este modelo. El resultado se guarda primero en caché y como artefacto reutilizable; después se construye Full, se comprueban una ventana nativa y el perfil aislado, y se ejecutan Stems/CALIDAD con las bibliotecas del bundle. El presupuesto total es 240 minutos en un runner `macos-15-intel`; la limpieza se limita a fuentes/objetos propios y Xcodes no seleccionados de ese runner desechable. Esta ruta es experimental hasta que **todos** sus pasos pasen. No cambia ni sustituye los paquetes 5.0.0 publicados.

El instalador NSIS comprueba WebView2. Si falta, ejecuta el bootstrapper Evergreen oficial incluido, que descarga e instala la dependencia, y comprueba de nuevo su presencia antes de instalar la aplicación. Solo ese caso requiere Internet; si la descarga o instalación falla, muestra un error y detiene la instalación de la aplicación. Si WebView2 ya está instalado, no ejecuta el bootstrapper. El MSI está pensado para despliegues con WebView2 instalado previamente; no incorpora el runtime. Microsoft documenta la [distribución del Evergreen Runtime](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution). La presencia de Microsoft Edge por sí sola no satisface este requisito.

La implementación actual no invoca FFmpeg ni ejecuta los motores Python anteriores. Incluye conversión y variaciones nativas; la edición `full` añade separación vocal y CALIDAD con LibTorch y un modelo TorchScript. No copia el intérprete Python ni `libtorch_python`. Algunas dependencias compilan bibliotecas C/C++, por lo que el equipo de build necesita CMake y un compilador C/C++; SoundTouch necesita libclang para generar sus bindings. La aplicación instalada no necesita esos compiladores.

## Desarrollo y pruebas

Desde la raíz:

```sh
cargo run --locked -p yt-app --bin yt-ultimate
cargo run --locked -p xtask -- check
```

`start_app.command` (Mac) y `start_app.ps1` (Windows) arrancan este mismo ejecutable. `check` ejecuta formato, pruebas del workspace y Clippy. El proyecto de la interfaz y el empaquetado son Rust; el sistema operativo proporciona el motor WebView. No se requiere npm para esta aplicación.

## Crear instaladores locales

```sh
cargo install cargo-packager --version 0.11.8 --locked --root target/packager-tools
cargo run --locked -p xtask -- package
```

El comando compila en modo `release`, genera una configuración con rutas absolutas y ejecuta la versión fijada del empaquetador. El destino es `target/packages/<host>/<version>/`. Los iconos originales se conservan en `packaging/icons/`; el identificador sigue siendo `com.ytultimate.desktop`.

Opciones:

```sh
# Comprobar y generar la configuración, sin compilar ni empaquetar:
cargo run --locked -p xtask -- prepare
# Solo la aplicación Mac, sin crear una imagen de disco:
cargo run --locked -p xtask -- package --formats app
# Solo NSIS en Windows:
cargo run --locked -p xtask -- package --formats nsis
```

Puede indicarse una instalación existente mediante `CARGO_PACKAGER`, que debe apuntar al ejecutable `cargo-packager` 0.11.8. Por defecto el comando empaqueta la arquitectura del compilador anfitrión y usa explícitamente `target/release`, aunque haya otra carpeta de salida configurada externamente. Windows base enlaza el CRT estáticamente; Windows `full` usa el CRT dinámico compatible con LibTorch y lo incluye junto al ejecutable.

Los paquetes locales no tienen firma de distribución. El comando comprueba que aparezcan los formatos solicitados y que los archivos no estén vacíos. En Mac valida `Info.plist`, arquitectura con `lipo`, dependencias con `otool`, ausencia de runtimes heredados e integridad del DMG con `hdiutil verify`. No abre, instala ni publica automáticamente los paquetes.

Cada generación de instaladores escribe también `SHA256SUMS` con el hash SHA-256 y el nombre exacto de los archivos.

Al preparar NSIS, `xtask` descarga el bootstrapper desde el enlace oficial de Microsoft y lo guarda en `target/native-deps/webview2/`; también admite `YTU_WEBVIEW2_BOOTSTRAPPER` para usar un archivo existente. Su SHA-256 queda fijado en `crates/xtask/src/windows.rs`; si Microsoft cambia el archivo, el empaquetado exige revisar y actualizar ese hash. En un anfitrión Windows se comprueba además su firma Authenticode de Microsoft. El bootstrapper se incluye en el área temporal del instalador, no entre los archivos permanentes de la aplicación. No se ejecuta al construir el paquete en Mac.

Para repetir las verificaciones de los paquetes existentes sin reconstruirlos:

```sh
cargo run --locked -p xtask -- verify
```

## Compilación cruzada Mac → Windows x64

La ruta opcional usa [cargo-xwin](https://github.com/rust-cross/cargo-xwin) 0.23.1, Clang/LLVM, LLD, CMake, Ninja y NSIS. Descarga los headers y bibliotecas de Microsoft MSVC/Windows SDK; no requiere ni incorpora Python. En Mac con Homebrew:

```sh
brew install llvm lld nsis cmake ninja
rustup target add x86_64-pc-windows-msvc
cargo install --locked cargo-xwin --version 0.23.1 --root target/packager-tools
export PATH="$(brew --prefix llvm)/bin:$(brew --prefix lld)/bin:$PATH"
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"
cargo run --locked -p xtask -- package --target x86_64-pc-windows-msvc
```

`CARGO_XWIN` permite indicar el ejecutable cargo-xwin. Esta ruta produce únicamente NSIS sin firma en `target/packages/x86_64-pc-windows-msvc/<version>/`, con el binario en `target/x86_64-pc-windows-msvc/release`. El empaquetador valida que el ejecutable sea PE32+ x64 y use el subsistema gráfico. MSI y firma de Windows se realizan en el runner Windows.

Compilar y analizar el instalador desde Mac no demuestra que su instalación, sus asociaciones o el WebView funcionen en Windows real. La prueba del instalador publicado se realizó después en un runner Windows real, como se detalla abajo; el análisis de macOS por sí solo no la sustituye.

El parche local `vendor/mp3lame-sys` conserva el código y licencias de LAME y selecciona la compilación MSVC por destino. `vendor/torch-sys` corrige un argumento MSBuild usado como argumento del compilador, sin modificar la API nativa. La plantilla NSIS local corrige la limpieza de asociaciones al desinstalar y entrecomilla las rutas con espacios. Windows base solo enlaza DLL del sistema; la edición `full` incluye además LibTorch y su CRT, sin Python ni Node.

## Paquete Mac con separación vocal

El modelo exportado no está en Git. Debe existir `crates/yt-media/models/bs-roformer-v1.pt`, o indicarse su ubicación mediante `YTU_PACKAGE_STEMS_MODEL`. El empaquetador exige el SHA-256 documentado en `crates/yt-media/models/README.md`. `LIBTORCH` debe apuntar a una distribución nativa LibTorch 2.12 que contenga `include/` y `lib/` para la arquitectura de destino.

```sh
LIBTORCH=/ruta/absoluta/a/libtorch cargo run --locked -p xtask -- package --native-stems
cargo run --locked -p xtask -- verify --native-stems
```

El resultado queda en `target/packages/<host>/<version>/full/` para conservar también la edición base, y el DMG termina en `_full.dmg` para identificarla al actualizar. Contiene el modelo en `Contents/Resources/models/` y cuatro bibliotecas en `Contents/Frameworks`: `libtorch.dylib`, `libtorch_cpu.dylib`, `libc10.dylib` y `libomp.dylib`. Se modifican copias de esas bibliotecas para usar rutas internas a la aplicación; la instalación de LibTorch original queda intacta. La verificación recorre también dependencias transitivas y exige la arquitectura correcta.

Sin `--signed` se aplica firma ad-hoc después de modificar las rutas y antes de crear el DMG. No se activa hardened runtime en esa firma local: una identidad ad-hoc no tiene Team ID para validar bibliotecas; los paquetes `--signed` sí conservan hardened runtime con Developer ID. La firma local permite verificar integridad, pero no representa una identidad de distribución ni una notarización. Esta edición declara macOS 14 como mínimo por sus bibliotecas nativas. No debe necesitar `DYLD_LIBRARY_PATH`, `LIBTORCH` ni una instalación Python para arrancar en el equipo de destino.

La separación incluida estima voces e instrumental, con CPU y salida WAV estéreo a 44,1 kHz; no equivale a todos los motores y opciones históricos. CALIDAD usa ese resultado, DSP y exportación FLAC24 verificada.

## Paquete Windows completo

Usar la distribución **Release x64 CPU de LibTorch 2.12.0 para Windows**, disponible en el [índice oficial](https://download.pytorch.org/libtorch/cpu), y el mismo modelo. `YTU_VC_RUNTIME_DIR` debe contener DLL x64 de Microsoft de una versión compatible: `msvcp140.dll`, `vcruntime140.dll` y `vcruntime140_1.dll`. Pueden obtenerse del directorio redistribuible de Visual Studio o del [redistribuible oficial de Microsoft](https://aka.ms/vs/17/release/vc_redist.x64.exe). La verificación local usó la versión 14.44.35211; no se copia un runtime de depuración.

```powershell
$env:LIBTORCH = 'C:\native-deps\libtorch'
$env:YTU_VC_RUNTIME_DIR = 'C:\native-deps\vc-runtime-x64'
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
cargo run --locked -p xtask -- package --native-stems --formats nsis
```

Para generar la misma edición desde Mac, establecer esas variables con rutas a las distribuciones **Windows**, además del PATH de LLVM/LLD, y añadir `--target x86_64-pc-windows-msvc`. El resultado es `target/packages/x86_64-pc-windows-msvc/<version>/full/yt-ultimate_<version>_x64_full-setup.exe`.

El modelo se instala en `models/` y las DLL junto a `yt-ultimate.exe`. El programa de empaquetado comprueba que los binarios sean PE32+ x64 y que todos sus imports normales y diferidos se resuelvan a DLL incluidas o del sistema Windows. Excluye `torch_python.dll` y el runtime Python. Estas comprobaciones se complementan con la instalación y el arranque reales del instalador publicado en Windows Server 2022 mediante Actions, descritos abajo. La edición `full` no genera MSI.

## Firma y notarización de macOS

Para distribuir fuera de la Mac que compila se necesita una identidad **Developer ID Application** y una cuenta Apple Developer con acceso a notarización. La firma usa hardened runtime. `--signed` exige tanto identidad como credenciales de notarización y falla si falta alguna; la firma ad-hoc `-` no cuenta como firma de distribución.

Configurar `APPLE_SIGNING_IDENTITY` y una de estas alternativas:

- `APPLE_KEYCHAIN_PROFILE`, con credenciales ya guardadas mediante `xcrun notarytool store-credentials`.
- `APPLE_ID`, `APPLE_PASSWORD` (contraseña específica de aplicación) y `APPLE_TEAM_ID`.
- `APPLE_API_KEY` (identificador de la clave), `APPLE_API_ISSUER` y `APPLE_API_KEY_PATH` (archivo `.p8`).

Si la identidad ya está en el llavero, no hace falta importarla. En CI, `APPLE_CERTIFICATE` contiene el certificado `.p12` en base64 y `APPLE_CERTIFICATE_PASSWORD` su contraseña. El empaquetador lo importa temporalmente. Nunca guardar estas credenciales en los archivos de configuración del proyecto.

```sh
cargo run --locked -p xtask -- package --signed
```

El comando exige que `codesign --verify --deep --strict` y `xcrun stapler validate` acepten el `.app`. La firma y notarización no se consideran probadas hasta completar ese comando con credenciales reales. Los nombres de variables son los de cargo-packager, que pueden diferir de los usados por Electron.

## Firma de Windows y MSI

Para `--signed`, importar el certificado de firma de código en `Cert:\CurrentUser\My` y configurar `WINDOWS_CERTIFICATE_THUMBPRINT` y `WINDOWS_TIMESTAMP_URL`. El Windows SDK debe aportar `signtool.exe`. Se utiliza SHA-256 y timestamp RFC 3161; el comando comprueba Authenticode de los instaladores resultantes.

El flujo de GitHub puede importar un PFX desde los secretos `WINDOWS_CERTIFICATE` (base64) y `WINDOWS_CERTIFICATE_PASSWORD`. Para certificados que requieren un token físico o servicio remoto, usar un runner con esa integración de firma; el flujo incluido no simula ese acceso.

El MSI se construye con WiX, requiere permisos administrativos e instala por equipo. NSIS instala para el usuario actual. Ambos usan la versión del workspace. No mezclar ambos canales para una misma instalación hasta probar actualizaciones y desinstalaciones. Mantener el identificador no garantiza por sí solo una actualización automática desde el instalador Electron; probar ese cambio de canal y la conservación del perfil en una máquina de prueba.

## Automatización y publicación

El flujo de GitHub ejecuta formato, pruebas, Clippy y compilación en Mac ARM64, Mac Intel y Windows x64. En ejecución manual o etiquetas `v*` genera instaladores y los sube únicamente como artefactos privados de la ejecución para revisión. Una ejecución manual con `signed=true` utiliza los secretos de firma. Las etiquetas generan paquetes sin firma para pruebas.

El flujo no publica releases, no modifica el repositorio de versiones anterior y no actualiza automáticamente instalaciones existentes. Revisar los artefactos y los cambios funcionales antes de una publicación.

El flujo adicional `.github/workflows/windows-full.yml` se ejecuta en los pull requests y manualmente. Tiene dos comprobaciones independientes sobre un runner nativo `windows-2022`:

- Descarga la prerelease pública 5.0.0 con SHA-256 fijado, instala silenciosamente en una carpeta temporal con espacios, abre la aplicación con perfil y biblioteca aislados, exige una ventana, un proceso WebView2 y la base de datos, la cierra normalmente y desinstala. Comprueba también los comandos de asociación y su retirada. Su resultado describe el **instalador publicado**, aunque las fuentes de la rama hayan avanzado.
- Compila y prueba las fuentes actuales con `native-stems`, crea NSIS Full en Windows y repite esa comprobación. Además ejecuta las pruebas nativas de separación de nueve segundos y CALIDAD→FLAC utilizando las bibliotecas y el modelo instalados. Para esas pruebas se retiran `LIBTORCH`, `LIBCLANG_PATH`, las rutas de desarrollo y las variables Python del proceso; solo quedan las rutas de Windows en `PATH`.

El segundo job también prepara una actualización desde el instalador original 4.4.1 con hash fijado. La nueva plantilla NSIS reconoce su identificador de Electron-builder, conserva la carpeta de instalación y ejecuta el desinstalador anterior con `--updated` después de asegurar WebView2. Rechaza una aplicación anterior abierta en vez de cerrarla forzosamente. El fixture CI crea un perfil SQLite v4 mediante el código Database original y comprueba pistas gestionadas y referenciadas, orden de playlist, preferencias y configuración de plugins, copia `pre-rust-*.sqlite3`, hashes de archivos y conservación del perfil tras desinstalar Rust. También comprueba accesos directos/AppUserModelID y descubrimiento de `%APPDATA%\YT Ultimate` sin `YTU_DATA_DIR`. El fixture Python es una herramienta de CI: no se distribuye ni se ejecuta con la aplicación instalada.

Esta ruta de actualización se limita a las instalaciones originales **4.x por usuario** con el identificador conocido; no elimina carpetas manualmente ni migra instalaciones por equipo desconocidas. Está en validación en CI y no forma parte del instalador 5.0.0 ya publicado, que se conserva inmutable. No se activa la actualización estable desde 4.x mientras esa prueba esté pendiente.

La preparación utiliza LibTorch 2.12 CPU oficial y el modelo extraído del instalador publicado, ambos con hash fijado, más las tres DLL redistribuibles de Visual Studio del runner. Los resultados JSON y registros se guardan como artefactos de Actions junto al instalador recién construido. El flujo no modifica la prerelease 5.0.0 ni sus archivos. Estas comprobaciones no certifican reproducción audible, interacción manual completa ni una máquina Windows de usuario sin herramientas de desarrollo instaladas.

Los labels y prerrequisitos se contrastaron con la [referencia de runners de GitHub](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) y el [inventario oficial Windows 2022](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md). La [prueba del instalador publicado](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42518474582/job/128846564064) terminó satisfactoriamente en Windows Server 2022: instalación silenciosa, ventana «YT Ultimate», proceso WebView2, SQLite aislado, asociaciones, cierre normal y desinstalación. El mismo run ejecutó satisfactoriamente las 84 pruebas Full de Windows; se detuvo después en una advertencia nueva de Clippy 1.99, corregida para la siguiente ejecución. El [run base completo](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42520151859) aprobó formato, pruebas, Clippy y compilación release en Mac ARM, Mac Intel (83 pruebas) y Windows (82). El siguiente run Full también aprobó sus 84 pruebas y Clippy, pero se detuvo en la comprobación externa del modelo durante el empaquetado. El commit 484a756 reemplaza ese cálculo externo por SHA-256 incremental en Rust, con dos pruebas del empaquetador y Clippy aprobados. El commit 663b806 corrige además el entorno de Windows PowerShell al invocarlo desde Cargo iniciado por PowerShell 7: elimina `PSModulePath` únicamente en el proceso hijo, que vuelve a cargar sus módulos propios de firma. Microsoft documenta esta [diferencia entre las rutas de módulos de ambas versiones](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_psmodulepath?view=powershell-7.6). Una prueba Windows ejercita los módulos reales; el preflight de firma y dependencias ya pasó en el [run 42523580440](https://github.com/ElWapoteDev/yt-ultimate-releases/actions/runs/42523580440). Ese mismo run completó después la compilación Full y el NSIS, la instalación/arranque/cierre/desinstalación y los probes de separación CPU y CALIDAD→FLAC usando exclusivamente las DLL y el modelo instalados. La prueba de actualización desde Electron falló porque el detector esperaba un nombre de registro sin versión, mientras Electron-builder registra «YT Ultimate 4.4.1». El detector ahora admite ambos nombres exactos y conserva la comprobación 4.x; su validación completa se repite en el siguiente run. Estos resultados pertenecen al snapshot 5.0.0 con correcciones CI: el código integrado 5.0.1, sus códecs adicionales y las funciones nuevas de escritorio requieren su propia ejecución.

Cuando se autorice publicar una versión, el comprobador Rust espera los instaladores en `ElWapoteDev/yt-ultimate-releases` y un asset `SHA256SUMS` con líneas `<sha256 en minúsculas>  <nombre exacto>`. Combinar los manifiestos de los destinos que se publiquen. Los nombres incluyen `YT Ultimate_<versión>_aarch64.dmg`, `YT Ultimate_<versión>_x64.dmg`, la variante `YT Ultimate_<versión>_aarch64_full.dmg`, `yt-ultimate_<versión>_x64-setup.exe` y `yt-ultimate_<versión>_x64_full-setup.exe`. Mantener la versión alineada con Cargo. Si GitHub proporciona `digest`, debe coincidir con el manifiesto. El actualizador mantiene la edición base/full elegida sin sustituir una por otra.

## Verificación realizada

Instaladores finales generados y comprobados el 8 de noviembre de 2026:

| Edición | Archivo dentro de `target/packages/` | Tamaño exacto |
| --- | --- | ---: |
| Mac Full, Apple Silicon, macOS 14+ | `aarch64-apple-darwin/5.0.0/full/YT Ultimate_5.0.0_aarch64_full.dmg` | 316.630.302 bytes |
| Windows Full, x64 | `x86_64-pc-windows-msvc/5.0.0/full/yt-ultimate_5.0.0_x64_full-setup.exe` | 309.303.263 bytes |

Cada carpeta contiene su `SHA256SUMS`. SHA-256 Mac: `b900e381f7a300ad8ef85b7e8036e0156abe69fe27a044bff9a4d9593478f632`. SHA-256 Windows: `39a0a4ffc8222af8909ad35f6b3d49a376afd1e5de8290d5d38c00151299568c`.

- La entrega local final utiliza únicamente las ediciones `full` bajo `target/packages/<destino>/5.0.0/full/`. Los paquetes base creados antes de las últimas funciones y correcciones se movieron a `target/intermediate-packages/<destino>/5.0.0-base-pre-final/`, con un aviso `NO_DISTRIBUIR.md`; se conservan como pruebas intermedias y no deben entregarse como versión final.
- Compilación y prueba unitaria del programa de empaquetado: ejecutadas localmente en Mac ARM64.
- Generación de configuración con las rutas reales del proyecto: ejecutada localmente.
- Configuración contrastada con cargo-packager 0.11.8 y su esquema distribuido.
- Compilación release Mac ARM64 y creación real de `.app`/`.dmg`: ejecutadas localmente. La edición base intermedia enlaza solo a bibliotecas del sistema; la edición `full` añade las cuatro bibliotecas LibTorch indicadas arriba. Ambas pasaron las verificaciones de arquitectura e integridad del DMG.
- Arranque y pruebas visuales del ejecutable principal: consultar la verificación de la migración en el README.
- Windows x64 base y `full`: ejecutables release MSVC y NSIS generados realmente mediante compilación cruzada; cabeceras PE32+ y subsistema gráfico comprobados. Base usa DLL del sistema; `full` incluye LibTorch, CRT y modelo, con imports transitivos validados. Se contrastaron 1.559 referencias a símbolos contra los exports de las diez DLL nativas incluidas. La integridad del archivo NSIS final pasó `7zz t`; su contenido incluye el ejecutable, modelo, bibliotecas, licencias y componentes del instalador, sin Electron, Node, Python ni `torch_python.dll`. Posteriormente el instalador publicado se instaló, arrancó y desinstaló satisfactoriamente en Windows Server 2022 mediante Actions; la evidencia enlazada arriba corresponde a ese archivo exacto.
- El NSIS final incluye también el bootstrapper WebView2 oficial de 1.897.688 bytes. Se extrajeron por streaming el ejecutable, el modelo y el bootstrapper desde el instalador final y se comprobaron sus hashes. Se auditó el script NSIS generado: consulta las claves HKLM/HKCU de 32 bits, omite la descarga si el runtime existe, espera la instalación, revalida y aborta con error antes de copiar la aplicación si no pudo instalarlo. Compilar y auditar ese flujo no equivale a ejecutarlo en Windows.
- Mac con separación vocal: bundle y DMG generados; modelo y cuatro bibliotecas incluidos. Se detectó y corrigió la incompatibilidad entre firma ad-hoc y hardened runtime; el bundle corregido arrancó con un perfil temporal sin variables de LibTorch ni rutas externas de bibliotecas. Desde la interfaz se importó un audio de 15 segundos, se completó la separación y se comprobaron original, voces e instrumental. También se verificaron exportación FLAC desde el menú, instalación y ejecución de un plugin, consulta del actualizador, CALIDAD con salida FLAC y descarga completa a MP3 de 635 segundos mientras se navegaba a otra búsqueda.
- La suite del workspace con las funciones nativas completas terminó con 85 pruebas aprobadas; formato y Clippy estricto aprobados. Esas pruebas locales se complementan con la comprobación nativa del instalador publicado en Windows descrita arriba.
- Tras incorporar la última corrección de paginación se reconstruyó Mac Full, se verificaron nuevamente el bundle y DMG, y se comprobó un arranque aislado de 10 segundos, sin variables `LIBTORCH`/`DYLD_LIBRARY_PATH` ni errores de carga. El proceso de esa prueba se cerró después. Los cambios finales del bootstrapper Windows también pasaron la prueba existente de `xtask`, formato y Clippy estricto.
- Mac Intel base: compilación release, pruebas, formato y Clippy aprobados en el runner nativo; no se ha demostrado aún su arranque gráfico ni generado su DMG. MSI, firma de distribución y notarización siguen pendientes de ejecución y de certificados reales.

Referencias: [cargo-packager 0.11.8](https://docs.rs/cargo-packager/0.11.8/cargo_packager/), [configuración](https://docs.rs/cargo-packager/0.11.8/cargo_packager/config/struct.Config.html), [distribución Dioxus](https://dioxuslabs.com/learn/0.7/guides/deploy/), [runners de GitHub](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job).
