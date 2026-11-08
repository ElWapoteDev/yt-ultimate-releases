"""Minimal YT Ultimate Plugin API v1 example."""


def setup(api):
    @api.register_command(
        "greet",
        title="Send a greeting",
        description="Returns a configured greeting for the supplied name.",
    )
    def greet(payload):
        name = str(payload.get("name", "mundo")).strip() or "mundo"
        return {"message": f"{api.config['greeting']}, {name}!"}

    @api.register_hook("app.started")
    def on_app_started(event):
        api.log.info("Hello Example ready for event %s", event.event_id)

    @api.on_shutdown
    def shutdown():
        api.log.info("Hello Example stopped")
