"""Dependency-free validation for plugin JSON data and configuration."""

from __future__ import annotations

import math
import re
from dataclasses import dataclass
from typing import Any, Mapping


_JSON_TYPES = {"object", "array", "string", "number", "integer", "boolean", "null"}
_SCHEMA_KEYS = {
    "type",
    "title",
    "description",
    "default",
    "enum",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "pattern",
}


@dataclass(frozen=True, slots=True)
class ValidationIssue(ValueError):
    path: str
    reason: str

    def __str__(self) -> str:
        return f"{self.path}: {self.reason}"


def ensure_json_value(value: Any, path: str = "$") -> None:
    """Reject values that cannot be represented as portable strict JSON."""

    if value is None or isinstance(value, (str, bool)):
        return
    if isinstance(value, int):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValidationIssue(path, "numbers must be finite")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            ensure_json_value(item, f"{path}[{index}]")
        return
    if isinstance(value, Mapping):
        for key, item in value.items():
            if not isinstance(key, str):
                raise ValidationIssue(path, "object keys must be strings")
            ensure_json_value(item, f"{path}.{key}")
        return
    raise ValidationIssue(path, f"{type(value).__name__} is not JSON-compatible")


def validate_config_schema(schema: Mapping[str, Any]) -> None:
    """Validate the supported, intentionally small JSON Schema subset."""

    if not isinstance(schema, Mapping):
        raise ValidationIssue("$.config_schema", "must be an object")
    _validate_schema_node(schema, "$.config_schema", root=True)


def schema_defaults(schema: Mapping[str, Any]) -> dict[str, Any]:
    """Collect property defaults from an object config schema."""

    defaults: dict[str, Any] = {}
    properties = schema.get("properties", {})
    if not isinstance(properties, Mapping):
        return defaults
    for key, property_schema in properties.items():
        if not isinstance(property_schema, Mapping):
            continue
        if "default" in property_schema:
            defaults[key] = _clone_json(property_schema["default"])
        elif property_schema.get("type") == "object":
            nested = schema_defaults(property_schema)
            if nested:
                defaults[key] = nested
    return defaults


def validate_config(config: Mapping[str, Any], schema: Mapping[str, Any]) -> None:
    """Validate a complete plugin configuration against the supported schema."""

    if not isinstance(config, Mapping):
        raise ValidationIssue("$", "configuration must be an object")
    ensure_json_value(config)
    _validate_instance(dict(config), schema, "$")


def _validate_schema_node(
    schema: Mapping[str, Any],
    path: str,
    *,
    root: bool = False,
) -> None:
    unknown = set(schema) - _SCHEMA_KEYS
    if unknown:
        names = ", ".join(sorted(unknown))
        raise ValidationIssue(path, f"unsupported schema keyword(s): {names}")

    data_type = schema.get("type", "object" if root else None)
    if data_type is not None and data_type not in _JSON_TYPES:
        raise ValidationIssue(f"{path}.type", f"unsupported type {data_type!r}")
    if root and data_type != "object":
        raise ValidationIssue(f"{path}.type", "root configuration type must be object")

    for field in ("title", "description", "pattern"):
        if field in schema and not isinstance(schema[field], str):
            raise ValidationIssue(f"{path}.{field}", "must be a string")
    if "pattern" in schema:
        try:
            re.compile(schema["pattern"])
        except re.error as exc:
            raise ValidationIssue(f"{path}.pattern", f"invalid regex: {exc}") from exc

    for field in ("minimum", "maximum"):
        if field in schema and (
            not isinstance(schema[field], (int, float))
            or isinstance(schema[field], bool)
            or not math.isfinite(schema[field])
        ):
            raise ValidationIssue(f"{path}.{field}", "must be a finite number")
    for field in ("minLength", "maxLength", "minItems", "maxItems"):
        if field in schema and (
            not isinstance(schema[field], int)
            or isinstance(schema[field], bool)
            or schema[field] < 0
        ):
            raise ValidationIssue(f"{path}.{field}", "must be a non-negative integer")
    for minimum, maximum in (
        ("minimum", "maximum"),
        ("minLength", "maxLength"),
        ("minItems", "maxItems"),
    ):
        if minimum in schema and maximum in schema and schema[minimum] > schema[maximum]:
            raise ValidationIssue(path, f"{minimum} cannot exceed {maximum}")

    if "enum" in schema:
        enum = schema["enum"]
        if not isinstance(enum, list) or not enum:
            raise ValidationIssue(f"{path}.enum", "must be a non-empty array")
        ensure_json_value(enum, f"{path}.enum")

    properties = schema.get("properties")
    if properties is not None:
        if data_type not in (None, "object"):
            raise ValidationIssue(path, "properties requires type 'object'")
        if not isinstance(properties, Mapping):
            raise ValidationIssue(f"{path}.properties", "must be an object")
        for name, child in properties.items():
            if not isinstance(name, str) or not name:
                raise ValidationIssue(f"{path}.properties", "property names must be strings")
            if not isinstance(child, Mapping):
                raise ValidationIssue(f"{path}.properties.{name}", "must be an object")
            _validate_schema_node(child, f"{path}.properties.{name}")

    required = schema.get("required")
    if required is not None:
        if not isinstance(required, list) or any(
            not isinstance(item, str) or not item for item in required
        ):
            raise ValidationIssue(f"{path}.required", "must be an array of strings")
        if len(set(required)) != len(required):
            raise ValidationIssue(f"{path}.required", "must not contain duplicates")

    additional = schema.get("additionalProperties")
    if additional is not None and not isinstance(additional, bool):
        raise ValidationIssue(f"{path}.additionalProperties", "must be a boolean")

    items = schema.get("items")
    if items is not None:
        if data_type not in (None, "array"):
            raise ValidationIssue(path, "items requires type 'array'")
        if not isinstance(items, Mapping):
            raise ValidationIssue(f"{path}.items", "must be an object")
        _validate_schema_node(items, f"{path}.items")

    if "default" in schema:
        ensure_json_value(schema["default"], f"{path}.default")
        _validate_instance(schema["default"], schema, f"{path}.default")


def _validate_instance(
    value: Any,
    schema: Mapping[str, Any],
    path: str,
) -> None:
    data_type = schema.get("type")
    if data_type and not _matches_type(value, data_type):
        raise ValidationIssue(path, f"expected {data_type}, got {_type_name(value)}")
    if "enum" in schema and value not in schema["enum"]:
        raise ValidationIssue(path, f"must be one of {schema['enum']!r}")

    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            raise ValidationIssue(path, f"must contain at least {schema['minLength']} characters")
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            raise ValidationIssue(path, f"must contain at most {schema['maxLength']} characters")
        if "pattern" in schema and re.search(schema["pattern"], value) is None:
            raise ValidationIssue(path, "does not match the required pattern")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema and value < schema["minimum"]:
            raise ValidationIssue(path, f"must be at least {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            raise ValidationIssue(path, f"must be at most {schema['maximum']}")

    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            raise ValidationIssue(path, f"must contain at least {schema['minItems']} items")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            raise ValidationIssue(path, f"must contain at most {schema['maxItems']} items")
        item_schema = schema.get("items")
        if isinstance(item_schema, Mapping):
            for index, item in enumerate(value):
                _validate_instance(item, item_schema, f"{path}[{index}]")

    if isinstance(value, Mapping):
        properties = schema.get("properties", {})
        required = schema.get("required", ())
        for name in required:
            if name not in value:
                raise ValidationIssue(f"{path}.{name}", "is required")
        if isinstance(properties, Mapping):
            for name, item in value.items():
                child_schema = properties.get(name)
                if isinstance(child_schema, Mapping):
                    _validate_instance(item, child_schema, f"{path}.{name}")
                elif schema.get("additionalProperties", True) is False:
                    raise ValidationIssue(f"{path}.{name}", "unknown configuration key")

def _matches_type(value: Any, data_type: str) -> bool:
    return {
        "object": isinstance(value, Mapping),
        "array": isinstance(value, list),
        "string": isinstance(value, str),
        "number": isinstance(value, (int, float)) and not isinstance(value, bool),
        "integer": isinstance(value, int) and not isinstance(value, bool),
        "boolean": isinstance(value, bool),
        "null": value is None,
    }[data_type]


def _type_name(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, Mapping):
        return "object"
    if isinstance(value, list):
        return "array"
    if isinstance(value, str):
        return "string"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    return type(value).__name__


def _clone_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _clone_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_clone_json(item) for item in value]
    return value


__all__ = [
    "ValidationIssue",
    "ensure_json_value",
    "schema_defaults",
    "validate_config",
    "validate_config_schema",
]
