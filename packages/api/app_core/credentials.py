import unicodedata

from cryptography.fernet import Fernet, InvalidToken
from django.conf import settings


class CredentialDecryptionError(ValueError):
    pass


def encrypt_credential_secret(secret: str) -> str:
    return Fernet(settings.CREDENTIAL_ENCRYPTION_KEY.encode("ascii")).encrypt(
        secret.encode("utf-8")
    ).decode("ascii")


def decrypt_credential_secret(encrypted_secret: str) -> str:
    try:
        return Fernet(settings.CREDENTIAL_ENCRYPTION_KEY.encode("ascii")).decrypt(
            encrypted_secret.encode("ascii")
        ).decode("utf-8")
    except (InvalidToken, UnicodeDecodeError, UnicodeEncodeError) as error:
        raise CredentialDecryptionError("credential_secret_decryption_failed") from error


def validate_lower_kebab(label: str, value: str) -> str:
    if (
        not isinstance(value, str)
        or not 1 <= len(value) <= 64
        or value.startswith("-")
        or value.endswith("-")
        or "--" in value
        or any(
            not (
                character.isascii()
                and (
                    character.islower()
                    or character.isdigit()
                    or character == "-"
                )
            )
            for character in value
        )
    ):
        raise ValueError(f"{label} must use lower-kebab-case")
    return value


def validate_display_name(value: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value != value.strip()
        or len(value) > 160
        or unicodedata.normalize("NFC", value) != value
        or any(
            ord(character) < 32 or 127 <= ord(character) < 160
            for character in value
        )
    ):
        raise ValueError("credential displayName is invalid")
    return value


def normalize_bearer_token_input(value: str) -> str:
    if isinstance(value, str):
        value = value.strip()
        if value[:7].lower() == "bearer ":
            value = value[7:].strip()
        if value.lower() == "bearer":
            raise ValueError("MCP bearer token is missing")
    return validate_bearer_token(value)


def validate_bearer_token(value: str) -> str:
    if (
        not isinstance(value, str)
        or not 1 <= len(value) <= 4096
        or not value.isascii()
        or any(not 0x21 <= ord(character) <= 0x7E for character in value)
    ):
        raise ValueError("MCP bearer token is invalid")
    return value
