"""Compatibility module for ASGI servers and older development scripts."""

from ytultimate.app import app, create_app, run


if __name__ == "__main__":
    run()


__all__ = ["app", "create_app", "run"]
