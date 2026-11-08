"""Console entry point for the API and bundled separator shim."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import __version__
from .app import run


def _separator_requested() -> bool:
    executable = Path(sys.argv[0]).name.lower()
    if executable.startswith("audio-separator"):
        return True
    if len(sys.argv) > 1 and sys.argv[1] == "--audio-separator":
        del sys.argv[1]
        sys.argv[0] = "audio-separator"
        if hasattr(sys, "orig_argv"):
            sys.orig_argv = ["audio-separator", *sys.argv[1:]]
        main_module = sys.modules.get("__main__")
        if main_module is not None:
            main_module.__spec__ = None
        return True
    return False


def main() -> int:
    if _separator_requested():
        from audio_separator.utils.cli import main as separator_main

        separator_main()
        return 0

    parser = argparse.ArgumentParser(prog="yt-ultimate-backend")
    parser.add_argument(
        "--version",
        action="version",
        version=f"YT Ultimate {__version__}",
    )
    parser.parse_args()
    run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
