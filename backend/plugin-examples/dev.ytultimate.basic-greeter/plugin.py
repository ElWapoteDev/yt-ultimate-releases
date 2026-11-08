"""Ejemplo básico y configurable para YT Ultimate Plugin API v1."""


_NOMBRE_PREDETERMINADO = "mundo"
_LONGITUD_MAXIMA_NOMBRE = 80


def _normalizar_nombre(valor):
    """Devuelve un nombre seguro o un error compatible con JSON."""

    if valor is None:
        return _NOMBRE_PREDETERMINADO, None
    if not isinstance(valor, str):
        return None, {
            "code": "invalid_name_type",
            "field": "name",
            "message": "El nombre debe ser texto.",
        }

    nombre = " ".join(valor.split())
    if not nombre:
        return _NOMBRE_PREDETERMINADO, None
    if len(nombre) > _LONGITUD_MAXIMA_NOMBRE:
        return None, {
            "code": "name_too_long",
            "field": "name",
            "max_length": _LONGITUD_MAXIMA_NOMBRE,
            "message": "El nombre no puede superar 80 caracteres.",
        }
    return nombre, None


def setup(api):
    """Registra el comando y el ciclo de vida del plugin."""

    @api.register_command(
        "greet",
        title="Saludar",
        description="Genera un saludo con la configuración actual.",
    )
    def saludar(payload):
        nombre, error = _normalizar_nombre(payload.get("name"))
        if error:
            return {"ok": False, "error": error}

        configuracion = api.config
        saludo = configuracion["greeting"]
        puntuacion = configuracion["punctuation"]
        mayusculas = configuracion["uppercase"]
        mensaje = f"{saludo}, {nombre}{puntuacion}"
        if mayusculas:
            mensaje = mensaje.upper()

        return {
            "ok": True,
            "message": mensaje,
            "name": nombre,
            "applied_config": {
                "greeting": saludo,
                "punctuation": puntuacion,
                "uppercase": mayusculas,
            },
        }

    @api.register_hook("app.started")
    def al_iniciar_aplicacion(event):
        version = event.payload.get("version", "desconocida")
        api.log.info(
            "Saludador básico listo para YT Ultimate %s (evento %s)",
            version,
            event.event_id,
        )

    @api.on_shutdown
    def apagar():
        api.log.info("Saludador básico detenido")
