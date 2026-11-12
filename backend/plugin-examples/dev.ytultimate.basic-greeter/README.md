# Saludador básico

Ejemplo autocontenido para YT Ultimate Plugin API v1. No tiene dependencias,
no solicita permisos y no modifica la biblioteca ni el sistema de archivos.

## Instalación

1. Copia este directorio completo al directorio de plugins configurado.
2. Conserva exactamente el nombre `dev.ytultimate.basic-greeter`.
3. Activa el modo desarrollador.
4. Revisa el código y habilita explícitamente **Saludador básico**.

## Comando

El comando `dev.ytultimate.basic-greeter.greet` acepta un objeto con el campo
opcional `name`:

```json
{
  "name": "Ada"
}
```

También funciona con `{}`, como cuando se ejecuta desde el menú. En ese caso
usa `mundo` y devuelve un resultado JSON:

```json
{
  "ok": true,
  "message": "Hola, mundo!",
  "name": "mundo",
  "applied_config": {
    "greeting": "Hola",
    "punctuation": "!",
    "uppercase": false
  }
}
```

Los nombres vacíos usan el valor predeterminado. Un tipo distinto de texto o
un nombre mayor de 80 caracteres produce un error JSON controlado.

## Configuración

- `greeting`: texto de 1 a 80 caracteres.
- `punctuation`: `!`, `.` o `✨`.
- `uppercase`: convierte el mensaje completo a mayúsculas.

El comando consulta `api.config` en cada ejecución, por lo que los cambios de
configuración se aplican sin recargar el plugin.

El hook `app.started` registra la versión de la aplicación. Al deshabilitar,
recargar o cerrar el plugin, su callback de apagado deja un registro de cierre.

Los plugins se ejecutan como código Python confiable dentro del proceso de YT
Ultimate; el manifiesto no es una sandbox de seguridad.
