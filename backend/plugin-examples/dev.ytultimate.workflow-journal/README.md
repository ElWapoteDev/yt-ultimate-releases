# Workflow Journal

Ejemplo avanzado y autocontenido para YT Ultimate Plugin API v1. Registra el
ciclo de vida de la aplicación, descargas, playlists y procesamiento en un
journal JSONL privado. No usa dependencias externas ni se instala o habilita
automáticamente.

## Seguridad

Los plugins de YT Ultimate son código Python confiable dentro del proceso; el
manifiesto no es una sandbox. Revisa los cuatro archivos antes de habilitarlo.

Workflow Journal sólo escribe dentro de `api.paths.data`, por lo que no solicita
permisos. Los archivos se crean con permisos privados cuando el sistema lo
permite y el estado se reemplaza de forma atómica. Aun así, un payload puede
contener información sensible:

- `redact_keys` reemplaza recursivamente valores de claves coincidentes por
  `[REDACTED]`, sin distinguir mayúsculas y minúsculas.
- `include_payload: false` evita guardar payloads por completo.
- La redacción compara nombres de clave exactos; revisa y amplía la lista para
  los eventos de tu instalación.
- Los cambios de redacción sólo afectan eventos futuros: no reescriben entradas
  ni exports que ya existan.
- Los archivos exportados contienen la misma información ya retenida en el
  journal. Protégelos y elimínalos cuando dejen de ser necesarios.

## Instalación

1. Copia este directorio completo al directorio de plugins configurado.
2. Conserva exactamente el nombre
   `dev.ytultimate.workflow-journal`.
3. Activa el modo desarrollador.
4. Revisa el código y habilita explícitamente **Workflow Journal**.

Los cambios de configuración se consultan mediante `api.config` en cada evento
y comando; no requieren recargar el plugin.

## Configuración

- `events`: lista de eventos que se guardan. Una lista vacía pausa el registro.
- `include_payload`: incluye el payload después de redactarlo.
- `max_entries`: conserva entre 100 y 50 000 entradas. Al reducirlo, el siguiente
  evento o consulta de estado aplica la nueva retención.
- `progress_step`: incremento entre `0.5` y `100` puntos porcentuales requerido
  para guardar otro `download.progress` del mismo trabajo.
- `redact_keys`: hasta 64 claves sensibles, comparadas con `casefold()`.
- `emit_summary_events`: emite un resumen al detener la app o completar una
  descarga, separación o fingerprint.

El primer progreso de cada descarga, una regresión de progreso y el progreso
`100` siempre se guardan. Los progresos omitidos se contabilizan en el estado.
Los resúmenes son independientes de `events`: un hito puede emitir su resumen
aunque se haya excluido del JSONL.

## Eventos observados

```text
app.started
app.stopping
download.queued
download.progress
download.media_ready
playlist.queued
playlist.progress
processing.stems.queued
processing.stems.progress
processing.stems.ready
processing.fingerprint.queued
processing.fingerprint.progress
processing.fingerprint.ready
```

Los hooks de inicio y apagado son `once`; todos tienen timeout y prioridad
explícitos. Los eventos frecuentes usan menor prioridad y un timeout más corto.

Cuando está habilitado `emit_summary_events`, el plugin emite:

```text
dev.ytultimate.workflow-journal.summary
```

El plugin sólo se suscribe a eventos concretos de la aplicación, nunca a su
propio namespace. Además conserva una guarda defensiva para que un cambio futuro
no introduzca recursión.

## Comandos

Todos los comandos reciben y devuelven valores compatibles con JSON. Campos no
documentados, tipos incorrectos y límites fuera de rango producen un error JSON
controlado.

### `dev.ytultimate.workflow-journal.status`

No acepta campos:

```json
{}
```

Devuelve rutas, tamaño, contadores, último evento y configuración efectiva.

### `dev.ytultimate.workflow-journal.recent`

Devuelve primero las entradas más nuevas. `limit` debe estar entre 1 y 500;
`event` debe ser exactamente uno de los eventos observados y `source` debe ser
un identificador válido.

```json
{
  "limit": 25,
  "event": "download.progress",
  "source": "core"
}
```

Los tres campos son opcionales; el límite predeterminado es 50.

### `dev.ytultimate.workflow-journal.export`

Crea un JSONL atómico dentro de `exports/`. Acepta los mismos filtros; su
`limit` opcional admite de 1 a 50 000 entradas. La ruta nunca la proporciona el
llamador, por lo que no existe escape fuera de `api.paths.data`.

```json
{
  "event": "processing.stems.ready",
  "limit": 1000
}
```

### `dev.ytultimate.workflow-journal.clear`

Requiere la frase exacta y no elimina exports anteriores:

```json
{
  "confirm": "BORRAR WORKFLOW JOURNAL"
}
```

### `dev.ytultimate.workflow-journal.emit-summary`

Comando asíncrono que emite el evento de resumen y devuelve el resultado del
dispatch. `reason` es opcional y admite de 1 a 200 caracteres:

```json
{
  "reason": "diagnóstico antes de actualizar el plugin"
}
```

## Archivos persistentes

Todos viven en el directorio privado entregado por `api.paths.data`:

```text
workflow-journal.jsonl
workflow-journal-state.json
exports/
└── workflow-journal-<fecha>-<id>.jsonl
```

Cada línea del journal contiene `schema_version`, `sequence`, `event`,
`event_id`, `source`, `emitted_at`, `recorded_at` y, si corresponde, `payload`.
El archivo de estado se actualiza con reemplazo atómico. Las escrituras, la
retención, las consultas, la exportación y el apagado están protegidos por un
`RLock`.

## Ejemplos con la API del host

Ejecutar una consulta tolerante a errores:

```python
resultado = await manager.run_command(
    "dev.ytultimate.workflow-journal.recent",
    {"limit": 10, "event": "download.media_ready"},
)
print(resultado.to_dict())
```

Ejecutar el diagnóstico asíncrono:

```python
resultado = await manager.run_command(
    "dev.ytultimate.workflow-journal.emit-summary",
    {"reason": "comprobación manual"},
)
```

Actualizar configuración sin recargar:

```python
configuracion = await manager.configure(
    "dev.ytultimate.workflow-journal",
    {"include_payload": False, "progress_step": 10},
)
```

Al deshabilitar, recargar o cerrar el plugin, el host retira primero hooks y
comandos. Después el callback de apagado persiste el estado final y cierra el
journal de forma idempotente.
