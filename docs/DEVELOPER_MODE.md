# Modo desarrollador y Plugin API v1

## Dos niveles de complejidad

YT Ultimate mantiene una interfaz sencilla por defecto:

- **Herramientas avanzadas** muestra procesamiento especializado como stems.
- **Modo desarrollador** habilita la administración de plugins, comandos y
  diagnósticos.

Son preferencias separadas. Ninguna es necesaria para buscar, descargar,
importar u organizar música.

## Advertencia de confianza

Un plugin de YT Ultimate es código Python ejecutado dentro del proceso del
backend. Puede importar módulos, leer o escribir archivos, acceder a la red o
iniciar procesos con la identidad del usuario aunque no use el SDK.

Los permisos del manifest son declaraciones auditables y gates para servicios
que el host decida exponer. **No son una sandbox ni impiden que Python acceda
directamente al sistema.** Instala sólo código que hayas revisado y en el que
confíes. Para código no confiable, usa un proceso separado con un protocolo IPC
explícito.

## Activar un plugin

1. Abre **Ajustes → Opciones avanzadas**.
2. Activa **Modo desarrollador** y acepta la advertencia.
3. Abre la carpeta de plugins desde Ajustes o
   **Desarrollo → Abrir carpeta de plugins**.
4. Coloca ahí una carpeta de plugin o créala con el scaffold.
5. Usa **Recargar plugins** para validar y descubrir los cambios.
6. Revisa el manifest y el código.
7. Habilita ese plugin de forma individual.

Activar el modo desarrollador no habilita código nuevo por sí mismo. Todo plugin
nuevo comienza deshabilitado; si el usuario ya había habilitado uno, esa elección
se conserva y el plugin se carga al volver a iniciar el runtime. Al apagar el
modo desarrollador se descargan todos los plugins inmediatamente sin borrar esa
elección.

## Ejemplos incluidos

La sección **Ajustes → Plugins → Ejemplos incluidos** ofrece dos proyectos
completos. **Instalar** sólo copia su código a la carpeta Plugins: no lo habilita,
no lo ejecuta y no reemplaza archivos existentes.

- `dev.ytultimate.basic-greeter` enseña manifest, esquema de configuración,
  comando, hook de arranque y teardown sin dependencias ni permisos especiales.
- `dev.ytultimate.workflow-journal` muestra una arquitectura avanzada con
  módulos separados, estado persistente, JSONL, redacción de secretos,
  retención, hooks de progreso, eventos propios y comandos síncronos/asíncronos.

Ambos incluyen un README con su contrato, ejemplos de uso y consideraciones de
seguridad. El código fuente vive en `backend/plugin-examples/` y también se
incluye como documentación dentro de la aplicación macOS.

## Crear un scaffold

La interfaz ofrece **Crear plugin…**. También se puede usar la herramienta del
backend desde la raíz del repositorio:

```bash
PYTHONPATH=backend backend/venv/bin/python -m ytultimate.plugins.scaffold \
  --plugin-dir "/ruta/mostrada/por/la-aplicacion/Plugins" \
  --name "Tag Tools" \
  dev.example.tags
```

El scaffold no sobrescribe una carpeta existente. Cada hijo inmediato de la
carpeta Plugins representa un plugin:

```text
Plugins/
└── dev.example.tags/
    ├── plugin.json
    ├── plugin.py
    └── helpers.py
```

La carpeta debe llamarse exactamente como el `id`. No se aceptan carpetas de
plugin ni entrypoints que sean symlinks.

## Manifest mínimo

```json
{
  "manifest_version": 1,
  "id": "dev.example.tags",
  "name": "Tag Tools",
  "version": "0.1.0",
  "api_version": 1,
  "entrypoint": "plugin.py:setup",
  "description": "Herramientas de metadatos configurables.",
  "permissions": [],
  "default_config": {
    "prefix": "[YT]"
  },
  "config_schema": {
    "type": "object",
    "properties": {
      "prefix": {
        "type": "string",
        "title": "Prefijo",
        "minLength": 0,
        "maxLength": 30,
        "default": "[YT]"
      }
    },
    "additionalProperties": false
  }
}
```

Son obligatorios `manifest_version`, `id`, `name`, `version`, `api_version` y
`entrypoint`.

- El ID empieza con letra minúscula y admite letras, números, `.`, `_` y `-`.
- `version` debe ser SemVer.
- La versión actual exige `manifest_version: 1` y `api_version: 1`.
- El entrypoint usa `archivo/relativo.py:callable`.
- Los campos desconocidos se rechazan, salvo extensiones con prefijo `x-`.
- Manifest, configuración, argumentos y resultados de comandos deben ser datos
  JSON estrictos.

El esquema de configuración admite el subconjunto documentado de JSON Schema:
`type`, `title`, `description`, `default`, `enum`, `properties`, `required`,
`additionalProperties`, `items`, límites numéricos, límites de longitud y
`pattern`. Una palabra clave no soportada produce un error visible; no se ignora.

## Primer plugin

```python
def setup(api):
    @api.register_command(
        "build_title",
        title="Construir título",
        description="Aplica el prefijo configurado sin modificar la biblioteca.",
        timeout=10,
    )
    def build_title(payload):
        title = str(payload.get("title", "")).strip()
        prefix = api.get_config("prefix", "")
        return {"title": f"{prefix} {title}".strip()}

    @api.register_hook("download.media_ready", priority=10)
    async def on_download(event):
        api.log.info("Descarga completada: %s", event.payload.get("file_id"))

    @api.on_shutdown
    def close_resources():
        api.log.info("Plugin detenido")
```

`setup(api)` puede devolver `None` o un callable adicional de teardown. Las
registraciones son transaccionales: si el import o setup falla, ningún comando
ni hook parcial se publica.

## Superficie estable

Importa únicamente nombres públicos de `ytultimate.plugins` y
`ytultimate.events`. Los módulos, atributos y clases que empiezan con `_` son
detalles internos.

`PluginAPI` ofrece:

- `api.api_version`, `api.plugin_id` y manifest inmutable;
- `api.config` y `api.get_config()` con copias defensivas;
- `api.paths.root`, `api.paths.data` y `api.paths.data_path()` seguro contra
  traversal accidental;
- `api.log`;
- `api.require_permission()`;
- `api.register_command()`;
- `api.register_hook()`;
- `api.on_shutdown`;
- `await api.emit()`.

El directorio `api.paths.data` es persistente y está dedicado al ID del plugin.
No es una frontera de seguridad. No escribas estado generado dentro del
directorio de código si puede ir ahí.

## Comandos

Un comando recibe un mapping JSON y puede devolver un valor JSON de forma
síncrona o asíncrona:

```python
@api.register_command("normalize", title="Normalizar")
async def normalize(payload):
    return {"value": str(payload.get("value", "")).strip().lower()}
```

El ID público siempre queda namespaced:
`dev.example.tags.normalize`. Los comandos activos aparecen en el menú
**Desarrollo → Comandos de plugins** y también pueden ejecutarse por la API
local:

```text
POST /api/v1/developer/plugins/{plugin_id}/commands/{command_name}
```

Los fallos y timeouts se convierten en errores estructurados con código,
mensaje, recuperabilidad, plugin y detalles; no se serializan tracebacks al
cliente.

## Hooks y eventos

```python
@api.register_hook(
    "library.file_added",
    priority=20,
    once=False,
    timeout=5,
)
async def added(event):
    await api.emit(
        "dev.example.tags.indexed",
        {"file_id": event.payload.get("file_id")},
    )
```

El envelope de evento es inmutable y contiene `name`, `payload`, `source`,
`emitted_at` y `event_id`. Los handlers de un evento se ejecutan
concurrentemente; la prioridad ordena sus resultados, no vuelve serial la
ejecución. El error de un hook no cancela a los demás.

El host actual publica estos eventos de trabajo:

| Familia | Eventos emitidos |
| --- | --- |
| Aplicación | `app.started`, `app.stopping` |
| Descargas | `download.queued`, `download.progress`, `download.media_ready` |
| Playlists remotas | `playlist.queued`, `playlist.progress` |
| Stems | `processing.stems.queued`, `processing.stems.progress`, `processing.stems.ready` |
| Fingerprint | `processing.fingerprint.queued`, `processing.fingerprint.progress`, `processing.fingerprint.ready` |
| Plugin | `plugin.loaded`, `plugin.disabled`, `plugin.config_changed` |

El host también publica `app.started` y `app.stopping`. La API v1 reserva nombres
convencionales para integraciones que los incorporen, como `download.completed`,
`download.failed`, `library.file_added`, `library.file_removed`,
`player.track_changed` y `player.state_changed`. No asumas que un evento opcional
se emite si el host no documenta esa integración. Los plugins deben tolerar
campos nuevos u opcionales. Nombra los eventos propios con el ID completo del
plugin para evitar colisiones.

## Configuración y estado

La configuración se combina con defaults, se valida y se escribe de forma
atómica. Un plugin cargado observa valores nuevos la siguiente vez que consulta
`api.config`; no necesita recarga para leerlos.

La UI y la API permiten:

- consultar configuración y schema;
- aplicar un patch o reemplazo;
- volver a defaults;
- habilitar, deshabilitar o recargar;
- refrescar el catálogo completo;
- consultar snapshots y diagnósticos.

El estado de habilitación y configuración usa `.plugin-state-v1.json` en la
carpeta Plugins con permisos restringidos y reemplazo atómico. No lo edites
mientras la aplicación está abierta.

## Lifecycle y recarga

Al deshabilitar, recargar, apagar el modo desarrollador o cerrar la aplicación:

1. se dejan de publicar comandos y hooks;
2. los callbacks de `api.on_shutdown` corren en orden inverso;
3. corre el teardown devuelto por `setup`;
4. se elimina el namespace aislado del módulo.

La recarga compila el entrypoint actual en un namespace nuevo y mantiene la
configuración persistida. Imports, setup, comandos, hooks y teardown tienen
timeouts. Python no puede terminar a la fuerza una función síncrona que ya está
corriendo en un thread; puede acabar después del timeout. Esto refuerza el
requisito de usar sólo plugins confiables.

## Dependencias de plugins

Plugin API v1 no instala dependencias ni crea un entorno virtual por plugin. Una
dependencia externa sólo estará disponible si forma parte del backend que se
ejecuta; no supongas que paquetes instalados en tu máquina existirán en una
aplicación empaquetada.

Para una extensión compleja o con dependencias propias, mantén un pequeño adapter
in-process y ejecuta el trabajo fuera de proceso mediante un protocolo explícito.
Ese proceso externo y su distribución son responsabilidad del plugin.

## Probar un plugin

Se recomienda probar primero handlers puros y después el lifecycle completo
contra un directorio temporal. El runtime ofrece `MemoryStateStore` para no tocar
preferencias reales:

```python
from pathlib import Path

from ytultimate.events import EventBus
from ytultimate.plugins import MemoryStateStore, PluginManager


async def exercise(plugin_directory: Path) -> None:
    manager = PluginManager(
        plugin_directory,
        developer_mode=True,
        state_store=MemoryStateStore(),
        event_bus=EventBus(),
        data_dir=plugin_directory / ".test-data",
    )
    await manager.start()
    await manager.enable("dev.example.tags")
    result = await manager.run_command(
        "dev.example.tags.build_title",
        {"title": "Canción"},
    )
    assert result.ok
    await manager.stop()
```

No uses la biblioteca real en pruebas. Crea plugins, datos y archivos en un
directorio temporal y verifica también fallo de manifest, timeout, disable,
reload y teardown.

## Diagnóstico

- Revisa el diagnóstico mostrado junto al plugin antes de intentar habilitarlo.
- Abre los logs desde el menú **Desarrollo**.
- Usa `api.log` en lugar de `print`.
- Comprueba que el nombre de la carpeta coincida con `id`.
- Comprueba que el entrypoint exista, sea `.py`, permanezca dentro del plugin y
  exporte el callable configurado.
- Si un cambio no aparece, usa la recarga explícita de plugins.

El contrato exhaustivo, el formato de error y todos los métodos del manager
están en [Plugin SDK v1](../backend/PLUGIN_SDK.md). El proyecto funcional
`backend/plugin-template/` sirve como referencia mínima. Los proyectos
`backend/plugin-examples/` pueden instalarse explícitamente desde Ajustes.
Ninguno se habilita automáticamente.
