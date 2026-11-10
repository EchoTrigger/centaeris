"""Private PostgreSQL Session payload storage; the public wire stays unchanged."""
from django.db import models
import json

STORAGE_KEY = "__centaerisSessionStorage"
STORAGE_SCHEMA = "workspace.session_event.storage.v1"


def _contains_nul(value):
    if isinstance(value, str):
        return "\0" in value
    if isinstance(value, dict):
        return any("\0" in key or _contains_nul(item) for key, item in value.items())
    if isinstance(value, list):
        return any(_contains_nul(item) for item in value)
    return False


def _query_index(value):
    if isinstance(value, str):
        return value.replace("\0", "\u2400")
    if isinstance(value, dict):
        return {key.replace("\0", "\u2400"): _query_index(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_query_index(item) for item in value]
    return value


def encode_session_payload(value):
    if not isinstance(value, dict) or STORAGE_KEY in value:
        raise ValueError("session_payload_storage_invalid")
    if not _contains_nul(value):
        return value
    stored = _query_index(value)
    stored[STORAGE_KEY] = {"schema": STORAGE_SCHEMA,
        "canonicalJson": json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))}
    return stored


def decode_session_payload(value):
    if not isinstance(value, dict) or STORAGE_KEY not in value:
        return value
    storage = value[STORAGE_KEY]
    if (not isinstance(storage, dict) or set(storage) != {"schema", "canonicalJson"}
            or storage["schema"] != STORAGE_SCHEMA or not isinstance(storage["canonicalJson"], str)):
        raise ValueError("session_payload_storage_invalid")
    try:
        wire = json.loads(storage["canonicalJson"])
    except (ValueError, TypeError) as error:
        raise ValueError("session_payload_storage_invalid") from error
    if not isinstance(wire, dict) or STORAGE_KEY in wire or not _contains_nul(wire):
        raise ValueError("session_payload_storage_invalid")
    actual = {key: item for key, item in value.items() if key != STORAGE_KEY}
    expected = _query_index(wire)
    if json.dumps(actual, sort_keys=True, separators=(",", ":")) != json.dumps(expected, sort_keys=True, separators=(",", ":")):
        raise ValueError("session_payload_storage_index_mismatch")
    return wire


class SessionPayloadField(models.JSONField):
    def get_db_prep_value(self, value, connection, prepared=False):
        stored = encode_session_payload(value) if isinstance(value, dict) else _query_index(value)
        return super().get_db_prep_value(stored, connection, prepared)

    def from_db_value(self, value, expression, connection):
        return decode_session_payload(super().from_db_value(value, expression, connection))
