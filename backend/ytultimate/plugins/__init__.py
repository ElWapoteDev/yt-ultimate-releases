"""YT Ultimate's opt-in developer plugin SDK and runtime.

Plugin API v1 consists of the public names exported here and from ``sdk``.
"""

from pathlib import Path

from .errors import PluginError, PluginErrorCode
from .examples import (
    PluginExample,
    discover_plugin_examples,
    install_plugin_example,
)
from .manager import (
    PluginDiagnostic,
    PluginManager,
    PluginSnapshot,
    PluginStatus,
)
from .manifest import (
    MANIFEST_VERSION,
    SDK_API_VERSION,
    PluginManifest,
)
from .sdk import (
    CommandDescriptor,
    CommandExecutionResult,
    CommandHandler,
    HookHandler,
    PluginAPI,
    PluginPaths,
    TeardownHandler,
)
from .state import JsonStateStore, MemoryStateStore, PluginState, StateStore


def create_plugin_scaffold(
    plugin_dir: Path | str,
    plugin_id: str,
    *,
    name: str | None = None,
    description: str = "A YT Ultimate developer plugin.",
) -> Path:
    """Lazily import the generator so ``python -m ...scaffold`` stays warning-free."""

    from .scaffold import create_plugin_scaffold as create

    return create(
        plugin_dir,
        plugin_id,
        name=name,
        description=description,
    )


__all__ = [
    "CommandDescriptor",
    "CommandExecutionResult",
    "CommandHandler",
    "HookHandler",
    "JsonStateStore",
    "MANIFEST_VERSION",
    "MemoryStateStore",
    "PluginAPI",
    "PluginDiagnostic",
    "PluginError",
    "PluginErrorCode",
    "PluginExample",
    "PluginManager",
    "PluginManifest",
    "PluginPaths",
    "PluginSnapshot",
    "PluginState",
    "PluginStatus",
    "SDK_API_VERSION",
    "StateStore",
    "TeardownHandler",
    "create_plugin_scaffold",
    "discover_plugin_examples",
    "install_plugin_example",
]
