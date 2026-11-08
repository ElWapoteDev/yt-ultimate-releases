# Plugins de ejemplo

Estos proyectos ejercitan la Plugin API v1 real y se distribuyen con la
aplicación:

- `dev.ytultimate.basic-greeter`: manifest, configuración dinámica, un comando,
  un hook y teardown.
- `dev.ytultimate.workflow-journal`: almacenamiento JSONL modular, estado
  atómico, redacción, retención, eventos, varios hooks y comandos síncronos y
  asíncronos.

## Instalación segura

Activa **Modo desarrollador** y usa **Ajustes → Plugins → Ejemplos incluidos**.
La aplicación valida el proyecto completo antes de copiarlo, rechaza symlinks y
nunca sobrescribe una carpeta que ya exista. El plugin instalado permanece
deshabilitado hasta que lo revises y lo actives expresamente.

También puedes copiar manualmente una de las carpetas completas al directorio
Plugins. Conserva su nombre exacto: debe coincidir con el `id` de `plugin.json`.

## Alcance de seguridad

Los permisos del manifest son declaraciones para servicios del SDK, no una
sandbox. Cualquier plugin Python se ejecuta dentro del backend con los permisos
del usuario. Revisa tanto el manifest como todo su código antes de habilitarlo.

Consulta [Plugin SDK v1](../PLUGIN_SDK.md) y
[Modo desarrollador](../../docs/DEVELOPER_MODE.md) para el contrato completo.
