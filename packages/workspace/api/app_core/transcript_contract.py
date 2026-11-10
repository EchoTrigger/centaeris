"""Structure comes from Core's generated serialization schema; behavior stays explicit."""
import json
from pathlib import Path

from jsonschema import Draft202012Validator

_SCHEMA = json.loads((Path(__file__).parent / "generated/transcript_block.schema.json").read_text(encoding="utf-8"))
Draft202012Validator.check_schema(_SCHEMA)
_BLOCK = Draft202012Validator(_SCHEMA)
_ENVELOPE_SCHEMAS = json.loads((Path(__file__).parent / "generated/transcript_envelopes.schema.json").read_text(encoding="utf-8"))
for _schema in _ENVELOPE_SCHEMAS.values():
    Draft202012Validator.check_schema(_schema)
_ENVELOPES = {name: Draft202012Validator(schema) for name, schema in _ENVELOPE_SCHEMAS.items()}


def valid_transcript_block_structure(value):
    return _BLOCK.is_valid(value)


def valid_transcript_structure(name, value):
    return _ENVELOPES[name].is_valid(value)
