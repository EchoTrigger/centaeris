"""Bounded private profile images, normalized before their Agent transaction commits."""
import base64
import binascii
import hashlib
import warnings
from io import BytesIO

from PIL import Image, ImageOps, UnidentifiedImageError


AVATAR_INPUT_MAX_BYTES = 1024 * 1024
AVATAR_MAX_PIXELS = 16_000_000
AVATAR_STORED_MAX_BYTES = 512 * 1024
AVATAR_SIZE = (256, 256)
AVATAR_FORMATS = ("PNG", "JPEG", "WEBP")


def normalize_avatar(encoded: str | None) -> tuple[bytes | None, str]:
    if encoded is None:
        return None, ""
    if len(encoded) > ((AVATAR_INPUT_MAX_BYTES + 2) // 3) * 4:
        raise ValueError("agent_avatar_too_large")
    try:
        content = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ValueError("agent_avatar_invalid") from error
    if not content or base64.b64encode(content).decode("ascii") != encoded:
        raise ValueError("agent_avatar_invalid")
    if len(content) > AVATAR_INPUT_MAX_BYTES:
        raise ValueError("agent_avatar_too_large")
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("error", Image.DecompressionBombWarning)
            with Image.open(BytesIO(content), formats=AVATAR_FORMATS) as image:
                width, height = image.size
                if width <= 0 or height <= 0 or width * height > AVATAR_MAX_PIXELS:
                    raise ValueError("agent_avatar_dimensions_invalid")
                if getattr(image, "n_frames", 1) != 1:
                    raise ValueError("agent_avatar_invalid")
                image.verify()
            with Image.open(BytesIO(content), formats=AVATAR_FORMATS) as image:
                image.load()
                oriented = ImageOps.exif_transpose(image)
                mode = "RGBA" if "A" in oriented.getbands() or "transparency" in oriented.info else "RGB"
                resized = ImageOps.fit(oriented.convert(mode), AVATAR_SIZE, method=Image.Resampling.LANCZOS)
                # A new image retains pixels only, including no EXIF/text/profile metadata.
                normalized = Image.new(mode, AVATAR_SIZE)
                normalized.paste(resized)
                output = BytesIO()
                normalized.save(output, format="PNG")
                data = output.getvalue()
    except ValueError as error:
        if str(error) in {"agent_avatar_invalid", "agent_avatar_dimensions_invalid"}:
            raise
        raise ValueError("agent_avatar_invalid") from error
    except (UnidentifiedImageError, OSError, SyntaxError, Image.DecompressionBombError,
            Image.DecompressionBombWarning) as error:
        raise ValueError("agent_avatar_invalid") from error
    if len(data) > AVATAR_STORED_MAX_BYTES:
        raise ValueError("agent_avatar_too_large")
    return data, hashlib.sha256(data).hexdigest()
