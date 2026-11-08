"""PyInstaller entry point kept outside the package for reliable imports."""

import multiprocessing

# PyTorch/audio-separator can start multiprocessing helpers while processing
# audio. In a frozen build those helpers re-enter this executable, so they must
# be diverted by PyInstaller before the application imports and parses CLI args.
multiprocessing.freeze_support()

from ytultimate.bootstrap import configure_ca_bundle

configure_ca_bundle()

from ytultimate.__main__ import main


if __name__ == "__main__":
    raise SystemExit(main())
