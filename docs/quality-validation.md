# CALIDAD: validación pendiente

## Condición de publicación

Por instrucción explícita del usuario, no publicar versiones hasta comprobar una carga completa y utilizable en Treblo (https://treblo.com/create), cuidando la calidad vocal. Un cambio de Chromaprint o un error genérico del servicio no satisface esta condición. El 7 de noviembre de 2026, después de los rechazos documentados abajo, el usuario autorizó «Permitir pequeños cambios en la voz». Esa autorización reemplaza el requisito de muestras vocales exactas para los siguientes experimentos; no autoriza deteriorar sustancialmente la voz ni garantiza aceptación. El perfil 5 y sus auditorías históricas siguen describiendo voz separada sin efectos.

4.5.0 se publicó antes de completar esa comprobación y fue devuelta a borrador. La versión pública actual vuelve a ser 4.4.1. No reutilizar una comprobación antigua de publicación de 4.5.0 como si siguiera pública. Si cambian los archivos después de esta retirada, usar una versión posterior (por ejemplo 4.5.1) al publicar: 4.5.0 fue visible durante un intervalo y pudo descargarse; no reemplazar silenciosamente su instalador con contenido distinto.

## Implementación y pruebas locales

- Perfil 5: ecualización dinámica de siete bandas, limitada a ±6 dB, exclusivamente sobre la instrumental.
- Voz separada a ganancia 1, sin cambios de tono o tiempo; auditoría muestra por muestra del FLAC PCM24 final, tolerancia de un paso de cuantización.
- 181 pruebas automatizadas y comprobación de 386 binarios Windows aprobadas. Prueba del separador real aprobada.
- Canción de Grupo Marrano: 239.699592 s, 10 570 752 fotogramas estéreo, auditoría vocal aprobada. Coincidencia local de bits 93.63 % anterior y 86.71 % nueva.
- Chiquilla: 159.159728 s, 7 018 944 fotogramas estéreo, auditoría vocal aprobada. Coincidencia local de bits 92.93 % anterior y 86.08 % nueva.
- Se prepararon WAV PCM24 con exactamente las mismas muestras decodificadas que los FLAC para comprobar compatibilidad de carga; no son variantes acústicas nuevas.

Los porcentajes anteriores no son probabilidades de reconocimiento ni medidas de protección. La auditoría se limita a la voz estimada por el separador; no recupera una grabación vocal original perfecta.

## Observaciones de Treblo

1. La carga anterior del usuario produjo HTTP 422 y `This upload matches existing audio and cannot be used.`
2. La copia nueva de Grupo Marrano se seleccionó en Safari. Durante la confirmación del usuario se inició la carga y después mostró `Your song was uploaded, but failed to process. Please try again.` El proyecto quedó con 0 pistas. Resultado de reconocimiento desconocido; no equivale a aceptación. El inspector cerrado no conservó la respuesta de esa petición.
3. Chiquilla, perfil 5: el selector nativo de Safari inicialmente resaltaba WAV y FLAC, pero mantenía deshabilitado Cargar. El usuario confirmó que la interacción manual habilitó Cargar. La copia WAV idéntica al FLAC quedó seleccionada y fue enviada una vez desde Safari; resultado: HTTP 422 en `/process/track_upload/185257b7-08b7-4ea8-91b4-718edd5ec0aa`, con `This upload matches existing audio and cannot be used.` La versión 5 no supera esta prueba.
4. Grupo Marrano, perfil 5, WAV PCM24 con todas las muestras idénticas al FLAC: enviado el 7 de noviembre de 2026; HTTP 422 en `/process/track_upload/afbedd21-b9a0-437d-8243-b86d2b8e8390`, con `This upload matches existing audio and cannot be used.` El proyecto muestra 0 pistas. Cambiar el contenedor no resuelve la carga; la causa exacta del error genérico anterior del FLAC sigue sin conocerse.

## Experimentos fuera de producción

- Diagnóstico de voz sola: stem separado de Chiquilla, sin efectos ni cambios de ganancia, exportado a WAV PCM24; conserva los 7 018 944 fotogramas con error máximo de un paso de cuantización. Enviado el 7 de noviembre de 2026; HTTP 422 en `/process/track_upload/d20f6b04-52de-4084-8526-6f08a7386626`, con `This upload matches existing audio and cannot be used.` Este stem separado basta para provocar el rechazo en esta prueba. No demuestra que todas las mezclas posibles sean rechazadas, que el stem esté libre de fugas instrumentales ni qué catálogo o motor utiliza el servicio.
- Variante de instrumental con cambio de octava de armónicos superiores y ecualización limitada: auditoría vocal aprobada, mismo número de fotogramas. La instrumental queda aproximadamente 4,1 dB más baja y pueden percibirse cambios de timbre o artefactos; no ha pasado una evaluación auditiva. Coincidencia local de bits 80,49 %. Enviada el 7 de noviembre de 2026; HTTP 422 en `/process/track_upload/4598ff91-56c8-4d55-b760-1a1fd36d57e8`, con `This upload matches existing audio and cannot be used.` No supera la prueba externa; no integrada en CALIDAD ni candidata a publicación.

La revisión local adicional estima picos entre muestras de +0,443 dBTP en el perfil 5 y +0,335 dBTP en la variante con octava y EQ. Ninguno excede la escala en sus muestras PCM, pero la comprobación de muestras no garantiza margen durante la reconstrucción analógica. La variante cambia el balance voz/instrumental unos 3,58 dB respecto al perfil 5. No considerar estos resultados una certificación perceptual de calidad. La voz aislada está prácticamente a techo (+0,034 dBTP estimado); modificar su nivel para ganar margen habría incumplido la condición original de conservarla a ganancia unitaria.

El 7 de noviembre de 2026 se retomó Safari tras el desbloqueo del usuario. Abrir el selector con un clic de coordenadas sobre el botón de archivo de la ventana principal y confirmar la ruta con «Ir a la carpeta» permitió habilitar Cargar. El diagnóstico de voz sola terminó con el rechazo explícito registrado arriba; se comprobó su petición nueva en el inspector, separada de la prueba anterior.

## Ensayo con pequeños cambios vocales autorizados

Después de la autorización nueva se preparó una única variante de Chiquilla sobre la mezcla del perfil 5. El remuestreo SoXR HQ variable se aplica conjuntamente a ambos canales y a toda la mezcla: tono base +0,28 semitonos, con variación suave limitada por dos senos de 13 y 23 segundos; sin octavas ni EQ adicional. Una ganancia constante común deja margen de pico sin cambiar el balance nominal de los stems.

La auditoría independiente del archivo final confirma tono medido +0,2002 a +0,3598 semitonos en ventanas de 50 ms, velocidad media +1,6267 %, duración 156,612063 s, ganancia −2,09885 dB y pico −1,1 dBTP. No hay muestras inválidas ni saturadas, la fuente permanece intacta y WAV/FLAC tienen las mismas muestras. El mapa temporal estimado es finito y creciente; no se presenta como una cronología exacta a nivel de muestra. Estas mediciones no equivalen a aceptación del detector ni a evaluación perceptual.

Se preparó una comparación de la misma frase de 25–37 s, con ambos fragmentos a −14,3 LUFS. El usuario confirmó que no notó deterioro vocal, corrigiendo su respuesta inicial. Treblo aceptó el archivo y abrió el editor con una pista de 2:36; se comprobó reproducción con avance hasta 0:06 y se dejó pausada. Proyecto `69fd9685-9546-4fe3-9680-428a70fee9f1`, pista `1f5336c9-9b94-406e-a27d-f5c0dce00503`. Esta aceptación corresponde solo al archivo experimental; no prueba todas las variantes ni identifica el proveedor del reconocimiento. Se inició la integración como perfil 6, pendiente de validar las salidas reales del servicio antes de publicar. La voz resultante no debe describirse como sin efectos o idéntica.

## Perfil 6 integrado y distribución 4.5.1

El servicio real completó ambas canciones con el perfil 6. Chiquilla produjo 6 906 592 fotogramas (156,612063 s), ganancia común −1,60865 dB y pico estimado −1,1 dBTP. Grupo Marrano produjo 10 400 614 fotogramas (235,841587 s), ganancia común −1,30059 dB y el mismo margen de pico. En ambos casos el WAV de prueba conserva exactamente las muestras decodificadas del FLAC de producción y los originales permanecen sin cambios. Estas salidas nuevas todavía necesitan su propia carga en Treblo; no heredan la aceptación del experimento anterior.

El instalador Windows 4.5.1 se construyó correctamente: 36 pruebas frontend, 144 backend y 10 E2E aprobadas; 386 binarios revisados sin problemas obligatorios. Se comprobó la coincidencia de los módulos críticos empaquetados con el código actual y de `latest.yml` con el instalador. Tamaño del instalador: 664 031 645 bytes; SHA256 `3cd178561c40cf96d8135b5bb2921112e3580e16f9822eb2bad3a892eb473178`. Esto no constituye una prueba de ejecución nativa en Windows. La publicación permanece pendiente de las dos comprobaciones externas.

## Revisión de una coincidencia por derechos

Los [términos de Treblo](https://treblo.com/tos) contemplan contenido propio o autorizado y ofrecen contacto con soporte en la sección 27. No se encontró una excepción pública del filtro por marcar la declaración de derechos, una lista de permitidos ni una apelación específica documentada para este mensaje. Solicitar revisión manual a soporte es una alternativa posible, no un desbloqueo garantizado. No se ha contactado a terceros. La respuesta HTTP observada tampoco identifica por sí sola al proveedor del detector o la grabación de referencia coincidente.

Mantener abierto el diagnóstico de red en la siguiente carga para registrar el resultado real. Hacer pruebas secuenciales, sin ráfagas ni repeticiones automáticas. No conservar cookies ni cabeceras de autenticación en el proyecto.

## Estado después de las pruebas del 7 de noviembre de 2026

El experimento de remapeo suave produjo una pista utilizable en Treblo y recibió aprobación auditiva del usuario. La variante de octava quedó descartada. Todavía no se ha publicado una versión nueva: primero se debe validar el perfil 6 integrado con Chiquilla y Grupo Marrano, incluyendo la calidad de los archivos finales y las pruebas de distribución. Los rechazos observados no permiten prometer que cambiar únicamente la instrumental resuelva el bloqueo: incluso el stem vocal separado fue rechazado. Esto es un límite observado, no una demostración de imposibilidad para todas las mezclas. Ante la aclaración solicitada, el usuario eligió permitir pequeños cambios vocales. Los siguientes experimentos deben registrar sus límites de tono y tiempo, sincronía entre voz e instrumental, niveles y resultado externo; ya no deben etiquetar esas voces como idénticas muestra por muestra.
