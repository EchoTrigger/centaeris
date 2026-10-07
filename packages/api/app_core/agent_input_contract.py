"""Hosted input identities and exact text; independent of Runtime intake types."""
import re


INPUT_ID_PATTERN = r"^[A-Za-z0-9_.:-]{1,64}$"
MAX_INPUT_BODY_BYTES = 65_536


def validate_input_id(value):
    if not isinstance(value, str) or re.fullmatch(INPUT_ID_PATTERN, value) is None:
        raise ValueError("agent_input_id_invalid")
    return value


def validate_input_body(value, *, allow_empty=False):
    if not isinstance(value, str) or (not allow_empty and not value.strip()) or "\0" in value:
        raise ValueError("agent_input_body_invalid")
    try:
        if len(value.encode("utf-8")) > MAX_INPUT_BODY_BYTES:
            raise ValueError("agent_input_body_invalid")
    except UnicodeError as error:
        raise ValueError("agent_input_body_invalid") from error
    return value
