# Modificación y verificación de huellas de audio

## Problema encontrado

La implementación anterior añadía 7–23 ms de silencio, cambiaba la frecuencia
de reproducción hasta un 0,02 %, compensaba el tempo y aplicaba ecualización
de hasta ±0,15 dB. También escribía un identificador en los metadatos. Esto
modificaba los bytes, pero no medía una huella acústica. La interfaz anunciaba
el inicio y refrescaba a los 2,5 y 8 segundos, sin esperar ni mostrar errores.

## Investigación

| Fuente primaria | Hallazgo relevante | Consecuencia para la implementación |
| --- | --- | --- |
| [Wang, ISMIR 2003](https://www.ee.columbia.edu/~dpwe/papers/Wang03-shazam.pdf) | El reconocimiento utiliza constelaciones de picos espectrales y relaciones temporales, con resistencia al ruido y distorsión. | Un hash de archivo o una etiqueta nueva no prueba un cambio de huella acústica. |
| [Haitsma y Kalker, ISMIR 2002](https://ismir2002.ircam.fr/proceedings/02-FP04-2.pdf) | Las huellas compactas están diseñadas para conservar la identificación bajo degradaciones del audio. | No se considera suficiente cambiar volumen, codificación o añadir un pequeño retraso. |
| [Six y Leman, Panako, ISMIR 2014](https://archives.ismir.net/ismir2014/paper/000122.pdf) | Hay algoritmos que reconocen audio incluso con cambios de tono y escala temporal. | Una huella distinta en Chromaprint no implica que otro sistema deje de reconocer la grabación. |
| [Singh et al., 2025](https://arxiv.org/abs/2511.05399) | Estudia huellas neuronales y entrenamiento con cambios de tono, tiempo, compresión y filtrado. | No se extrapolan resultados de un verificador a reconocedores modernos o propietarios. |
| [Chromaprint, documentación del proyecto](https://acoustid.org/chromaprint) y [README 1.6.2](https://github.com/acoustid/chromaprint/blob/v1.6.2/README.md) | Extrae huellas del audio; `fpcalc` ofrece salida JSON. Su objetivo es identificar audio casi idéntico. | Se integra un extractor real, con algoritmo 2 fijado y comparación explícita. |
| [FFmpeg, filtros oficiales](https://github.com/FFmpeg/FFmpeg/blob/master/doc/filters.texi) | `asetrate` afecta al tono y velocidad; `atempo` ajusta el tempo. | Se controla por separado el tono y la velocidad final, compensando la frecuencia entera realmente utilizada. |

Los valores del perfil son decisiones de ingeniería; los papers no validan
estos parámetros como un método universal de impedir reconocimiento.

## Comportamiento actual

### CALIDAD: cambios suaves y sincronizados

El menú **Modificar fingerprint → CALIDAD · FLAC** usa `intensity: "quality"`.
El perfil 6 separa voz e instrumental con BS-RoFormer a 44,1 kHz y aplica a la
instrumental una ecualización dinámica de siete bandas limitada a ±6 dB.
Después reúne ambas pistas y aplica a la mezcla un remuestreo SoXR con cambios
suaves y compartidos de tono y velocidad: +0,28 semitonos de base, variación
lenta de ±0,08 semitonos y velocidad media aproximadamente un 1,6 % mayor. La
canción queda algo más corta; voz e instrumental permanecen sincronizadas
porque comparten la misma transformación.

Una ganancia constante común deja margen de pico real estimado, con objetivo
de −1,1 dBTP y techo de −1 dBTP, medido con sobremuestreo de ocho veces. La
salida es FLAC de 24 bits. La comprobación final verifica las muestras de la
mezcla procesada dentro de un paso de cuantización de 24 bits y su margen de
pico; no demuestra que las muestras de la voz sean iguales a las anteriores.
La voz recibe los cambios conjuntos de tono, velocidad y volumen. La calidad se valora al
escuchar cada resultado, y la separación por IA puede dejar artefactos.

Los metadatos distinguen `processing_target: "instrumental_then_full_mix"`,
`vocals_preserved: false` y `vocal_processing: "shared_smooth_remap"`.
`shared_processing` registra el remuestreo, la velocidad media y la ganancia
compartida; `final_audio_integrity` registra la comprobación de la mezcla
exportada. La auditoría del stem vocal anterior al remuestreo queda en
`pre_remap_vocal_integrity` y no describe la voz del resultado final.
Los archivos temporales se limpian al terminar, fallar o cancelar. La huella
se compara sobre el archivo final frente a la fuente.

Los perfiles históricos 4–5 modificaban solo la instrumental y mantenían el
stem vocal sin efectos, con ganancia 1. El perfil 5 añadía una comprobación de
su contribución en el FLAC final dentro de la precisión de 24 bits. Esa
descripción sigue disponible para las copias antiguas y no se aplica al perfil
6. En ambos casos, un stem separado por IA no equivale a disponer de la pista
vocal original de estudio.

### Intenso y Equilibrado

1. Genera una copia, preservando el original, con un perfil reproducible por ID.
   El menú ofrece **Intenso** (+1,75 a +1,95 semitonos, predeterminado en la API)
   y **Equilibrado** (+1 a +1,2 semitonos). Ambos mantienen la velocidad de
   1,025 a 1,035 y la ecualización suave. Intenso aumenta la transposición sin
   añadir ruido ni incrementar también el cambio de tempo. Estos cambios son
   audibles; más intensidad no implica una distancia de huella necesariamente
   mayor para cualquier canción o algoritmo. Un limitador contiene los picos.
   La versión 3 conserva la frecuencia de muestreo del original y exporta
   siempre FLAC de 24 bits, evitando una nueva compresión con pérdidas. Esto
   no recupera información perdida en una fuente MP3/AAC ni elimina los
   posibles artefactos de modificar tono y tempo. Los archivos pueden ocupar
   más espacio. Conviene crear cada variante desde el original.
### Verificación común a los tres modos

1. Decodifica original y resultado a PCM estéreo, 44,1 kHz y 16 bits. Compara
   SHA-256 de las muestras completas, excluyendo etiquetas y contenedores.
   Si las muestras no cambian, el trabajo falla y elimina la salida parcial.
2. Ejecuta Chromaprint 1.6.2 localmente sobre hasta los primeros 120 segundos de
   cada archivo. No se sube audio a ningún servicio.
3. Compara los valores de las subhuellas en las posiciones correspondientes.
   Una diferencia de longitud por sí sola no cuenta como cambio acústico.
   Conserva el estado, número de subhuellas y hashes de estas en los metadatos.
4. Mide la duración del resultado antes de publicarlo. Actualiza la biblioteca
   al terminar y muestra progreso, resultado o error real.

Estados de verificación:

- `changed`: existen subhuellas distintas en las posiciones comparadas.
- `unchanged`: las subhuellas coinciden en el tramo común.
- `insufficient_audio`: alguna huella tiene menos de 16 subhuellas; no se afirma
  un resultado acústico a partir de una muestra tan corta.
- `unavailable`: no se encontró `fpcalc`. Se comprueba el PCM y se indica
  explícitamente que falta la verificación acústica.

Un error de un verificador configurado hace fallar el trabajo; no se convierte
silenciosamente en una verificación positiva. La cancelación también se respeta.

El estado `changed` compara subhuellas exactas en las mismas posiciones.
Además, `local_comparison` registra la coincidencia de bits sin desplazamiento
(`aligned`) y con el mejor desplazamiento global dentro de un intervalo
limitado (`best_offset`). Esta alineación no compensa variaciones de tempo ni
consulta un índice de grabaciones. **No mide una probabilidad de
reconocimiento**, ni demuestra que AcoustID, Shazam, Content ID, Panako, Audible
Magic u otro sistema no reconozcan el resultado. La aplicación no ejecuta una
prueba externa; el campo `recognition_tested` permanece en `false`.
La carpeta histórica `Protegidos` se mantiene por compatibilidad; su nombre
no representa una garantía de protección.

## Distribución y desarrollo

`stage-native-tools.mjs` incluye automáticamente `fpcalc` para macOS (universal
Intel/Apple Silicon) y Windows x64. Los archivos oficiales 1.6.2 están fijados
por SHA-256 y se incluyen licencias y referencias al código fuente.

Para preparar únicamente el verificador en desarrollo, desde `frontend`:

```sh
node scripts/stage-fingerprint-tool.mjs
# Para preparar el recurso de Windows desde otra plataforma:
node scripts/stage-fingerprint-tool.mjs --platform=win32
```

Electron pasa su ruta al backend. También se admite `YTU_FPCALC_PATH`, el
directorio de recursos `bin` y un `fpcalc` en PATH para ejecutar Python solo.

## Validación

Las pruebas de integración generan audio sintético estéreo con secuencias de
notas y ejecutan FFmpeg y Chromaprint reales. Comprueban PCM y huellas distintas
para entradas WAV, FLAC, MP3, M4A, AAC, OGG y Opus y salidas FLAC de 24 bits;
ambas intensidades con fuentes de 96 kHz; duración corregida; conservación del
original y los canales; cambio de tono medido mediante cruces por cero; y
rechazo de una copia que solo cambia metadatos. También prueban clips cortos,
errores, cancelación, limpieza y falta del verificador.

Las pruebas de interfaz recorren el menú de la biblioteca, la terminación real,
la aparición de la copia y la información de verificación. Las pruebas unitarias
del seguimiento cubren trabajos largos, desconexiones, cancelación y errores.

Las comprobaciones históricas de CALIDAD cubren la conservación del stem vocal
antes del remuestreo compartido, la ganancia aplicada a la instrumental y la
limpieza ante errores o cancelación. Para la versión 4.4.0 se ejecutó el flujo
completo con BS-RoFormer real sobre una señal sintética de 14 segundos a 48 kHz:
el resultado mantuvo los 14 segundos, produjo FLAC de 24 bits a 44,1 kHz y
obtuvo una huella Chromaprint distinta (92 subhuellas en ambos archivos).

La variante de remuestreo compartido incorporada al perfil 6 se evaluó también
con archivos reales: Treblo aceptó las variantes probadas y el usuario no
percibió deterioro en la escucha. El resultado se limita a esos archivos y a
esa prueba; no demuestra aceptación de otras canciones ni identifica el
proveedor del detector usado por Treblo. Tampoco constituye un estudio de
calidad perceptual sobre un catálogo musical. Cada archivo nuevo conserva el
estado de reconocimiento externo sin comprobar hasta que se evalúe por separado.
