# Hello Example

This is a functional YT Ultimate Plugin API v1 project, but it is never copied,
installed, or enabled automatically.

To try it:

1. Copy this entire directory to the configured plugin directory.
2. Rename the copied directory to exactly `dev.example.hello`.
3. Enable Developer Mode.
4. Review `plugin.py`, then explicitly enable **Hello Example**.

The plugin contributes the command `dev.example.hello.greet` and observes
`app.started`. See `../PLUGIN_SDK.md` for the complete contract and security
model.
