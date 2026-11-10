import unicodedata


MAX_AGENT_ID_CHARACTERS = 64
MAX_AGENT_NAME_CHARACTERS = 255
MAX_AGENT_DESCRIPTION_CHARACTERS = 128
MAX_AGENT_INSTRUCTIONS_CHARACTERS = 16_000


def validate_agent_id(value) -> str:
    if not isinstance(value, str) or value != value.strip() or not value:
        raise ValueError("agent_id_invalid")
    if len(value) > MAX_AGENT_ID_CHARACTERS:
        raise ValueError("agent_id_invalid")
    if unicodedata.normalize("NFC", value) != value:
        raise ValueError("agent_id_invalid")
    if "/" in value or "\\" in value:
        raise ValueError("agent_id_invalid")
    if any(unicodedata.category(character) in {"Cc", "Cs"} for character in value):
        raise ValueError("agent_id_invalid")
    return value


def normalize_agent_name(value) -> str:
    normalized = unicodedata.normalize("NFC", value.strip()) if isinstance(value, str) else ""
    if (
        not normalized
        or len(normalized) > MAX_AGENT_NAME_CHARACTERS
        or any(unicodedata.category(character) in {"Cc", "Cs"} for character in normalized)
    ):
        raise ValueError("agent_name_invalid")
    return normalized


def normalize_agent_description(value) -> str:
    normalized = unicodedata.normalize("NFC", value.strip()) if isinstance(value, str) else ""
    if len(normalized) > MAX_AGENT_DESCRIPTION_CHARACTERS or any(
        unicodedata.category(character) in {"Cc", "Cs"} for character in normalized
    ):
        raise ValueError("agent_description_invalid")
    return normalized


def normalize_agent_instructions(value) -> str:
    normalized = (
        unicodedata.normalize("NFC", value.replace("\r\n", "\n").replace("\r", "\n")).strip()
        if isinstance(value, str)
        else ""
    )
    if len(normalized) > MAX_AGENT_INSTRUCTIONS_CHARACTERS or any(
        unicodedata.category(character) in {"Cc", "Cs"}
        and character not in {"\n", "\t"}
        for character in normalized
    ):
        raise ValueError("agent_instructions_invalid")
    return normalized
