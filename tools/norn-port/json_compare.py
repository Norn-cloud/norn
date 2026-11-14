"""Shared type-preserving comparison for JSON parity and mutation evidence."""
from decimal import Decimal
import unicodedata


def canonical(value):
    """Norn wire strings are NFC; ordered lists stay ordered."""
    if isinstance(value, str):
        return unicodedata.normalize("NFC", value)
    if isinstance(value, dict):
        result = {canonical(key): canonical(item) for key, item in value.items()}
        if not equal(len(result), len(value)):
            raise ValueError("NFC key collision")
        return result
    if isinstance(value, list):
        return [canonical(item) for item in value]
    return value


def comparable(value):
    """Preserve JSON type distinctions while comparing canonical numbers."""
    if value is None:
        return ("null",)
    if isinstance(value, bool):
        return ("boolean", value)
    if isinstance(value, (int, float)):
        return ("number", Decimal(str(value)))
    if isinstance(value, str):
        return ("string", value)
    if isinstance(value, list):
        return ("array", tuple(comparable(item) for item in value))
    if isinstance(value, dict):
        return ("object", tuple(sorted((key, comparable(item)) for key, item in value.items())))
    raise TypeError(f"unsupported JSON type: {type(value).__name__}")


def equal(left, right):
    return comparable(canonical(left)) == comparable(canonical(right))


