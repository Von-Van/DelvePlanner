"""A small JSON Schema validator (stdlib only).

It supports the subset of draft 2020-12 that the benchmark's own schemas use: ``type``, ``properties``,
``required``, ``additionalProperties``, ``items``, ``enum``, ``const``, ``minimum``, ``maximum``,
``minLength``, ``maxLength``, ``minItems``, ``maxItems``, ``pattern``, ``anyOf``, ``oneOf``, ``not`` and local ``$ref``
(``#/$defs/...``). Anything else in a schema is ignored, so keep schemas inside this subset.
"""

from __future__ import annotations

import re
from typing import Any

_TYPES = {
    "object": dict,
    "array": list,
    "string": str,
    "boolean": bool,
    "null": type(None),
}


def _is_type(value: Any, name: str) -> bool:
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if name == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    return isinstance(value, _TYPES[name])


def validate(value: Any, schema: dict, root: dict | None = None, path: str = "$") -> list[str]:
    """Return a list of human-readable problems; an empty list means the value is valid."""
    root = root if root is not None else schema
    if "$ref" in schema:
        ref = schema["$ref"]
        if not ref.startswith("#/"):
            raise ValueError(f"only local $ref is supported, got {ref}")
        target: Any = root
        for part in ref[2:].split("/"):
            target = target[part]
        return validate(value, target, root, path)

    errors: list[str] = []
    expected = schema.get("type")
    if expected is not None:
        names = expected if isinstance(expected, list) else [expected]
        if not any(_is_type(value, n) for n in names):
            return [f"{path}: expected {'/'.join(names)}, got {type(value).__name__}"]

    if "const" in schema and (value != schema["const"] or type(value) is not type(schema["const"])):
        errors.append(f"{path}: expected {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: {value!r} is not one of {schema['enum']}")

    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            errors.append(f"{path}: shorter than {schema['minLength']}")
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            errors.append(f"{path}: longer than {schema['maxLength']}")
        if "pattern" in schema and not re.search(schema["pattern"], value):
            errors.append(f"{path}: {value!r} does not match {schema['pattern']}")

    if _is_type(value, "number"):
        if "minimum" in schema and value < schema["minimum"]:
            errors.append(f"{path}: {value} is below {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            errors.append(f"{path}: {value} is above {schema['maximum']}")

    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            errors.append(f"{path}: fewer than {schema['minItems']} items")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            errors.append(f"{path}: more than {schema['maxItems']} items")
        if "items" in schema:
            for i, item in enumerate(value):
                errors.extend(validate(item, schema["items"], root, f"{path}[{i}]"))

    if isinstance(value, dict):
        props = schema.get("properties", {})
        for key in schema.get("required", []):
            if key not in value:
                errors.append(f"{path}: missing required field {key!r}")
        extra = schema.get("additionalProperties", True)
        for key, item in value.items():
            if key in props:
                errors.extend(validate(item, props[key], root, f"{path}.{key}"))
            elif extra is False:
                errors.append(f"{path}: unexpected field {key!r}")
            elif isinstance(extra, dict):
                errors.extend(validate(item, extra, root, f"{path}.{key}"))

    if "not" in schema and not validate(value, schema["not"], root, path):
        errors.append(f"{path}: matches a shape that is not allowed")
    if "anyOf" in schema:
        if all(validate(value, s, root, path) for s in schema["anyOf"]):
            errors.append(f"{path}: matches none of the allowed shapes")
    if "oneOf" in schema:
        matches = sum(1 for s in schema["oneOf"] if not validate(value, s, root, path))
        if matches != 1:
            errors.append(f"{path}: matches {matches} shapes, expected exactly one")
    return errors
