"""Validate inline image bytes before projecting provider-specific content.

Limits are checked against the Core-generated model image corpus in CI.
Inspection reads image headers, matching Core; it does not decompress pixels.
"""

import base64
import binascii
import warnings
from io import BytesIO

from PIL import Image, UnidentifiedImageError


MODEL_INPUT_IMAGE_MAX_BYTES = 10 * 1024 * 1024
MODEL_INPUT_IMAGE_MAX_PIXELS = 100_000_000
IMAGE_FIELDS = {"messageId", "contentType", "placeholder", "dataBase64"}
CONTENT_TYPES = {"PNG": "image/png", "JPEG": "image/jpeg", "WEBP": "image/webp"}


def _inspect(image):
    encoded = image["dataBase64"]
    if len(encoded) > ((MODEL_INPUT_IMAGE_MAX_BYTES + 2) // 3) * 4:
        raise ValueError("model_input_image_byte_length_invalid")
    try:
        data = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ValueError("prepared_prompt_image_base64_invalid") from error
    if not data or base64.b64encode(data).decode("ascii") != encoded:
        raise ValueError("prepared_prompt_image_base64_invalid")
    if len(data) > MODEL_INPUT_IMAGE_MAX_BYTES:
        raise ValueError("model_input_image_byte_length_invalid")
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", Image.DecompressionBombWarning)
            with Image.open(BytesIO(data), formats=list(CONTENT_TYPES)) as decoded:
                width, height = decoded.size
                content_type = CONTENT_TYPES[decoded.format]
    except (UnidentifiedImageError, OSError, ValueError, Image.DecompressionBombError) as error:
        raise ValueError("prepared_prompt_image_content_invalid") from error
    if width <= 0 or height <= 0 or width * height > MODEL_INPUT_IMAGE_MAX_PIXELS:
        raise ValueError("model_input_image_dimensions_invalid")
    if content_type != image["contentType"]:
        raise ValueError("prepared_prompt_image_content_type_mismatch")


def project_images(prompt, messages, message_indexes, api):
    images = prompt.get("inputImages", [])
    if not isinstance(images, list):
        raise ValueError("prepared_prompt_image_invalid")
    raw_messages = prompt["messages"]
    by_id = {message["messageId"]: message for message in raw_messages}
    grouped, seen = {}, set()
    for image in images:
        if not isinstance(image, dict) or set(image) != IMAGE_FIELDS or any(not isinstance(image[key], str) or not image[key].strip() for key in IMAGE_FIELDS):
            raise ValueError("prepared_prompt_image_invalid")
        message = by_id.get(image["messageId"])
        if message is None:
            raise ValueError("prepared_prompt_image_message_missing")
        binding = (image["messageId"], image["placeholder"])
        if message["role"] != "user" or image["contentType"] not in CONTENT_TYPES.values() or binding in seen or message.get("content", "").count(image["placeholder"]) != 1:
            raise ValueError("prepared_prompt_image_invalid")
        seen.add(binding)
        _inspect(image)
        grouped.setdefault(image["messageId"], []).append(image)
    for message in raw_messages:
        bound = grouped.get(message["messageId"], [])
        if not bound:
            continue
        content, cursor, parts = message["content"], 0, []
        text_type = "input_text" if api == "openai_responses" else "text"
        for image in sorted(bound, key=lambda item: content.index(item["placeholder"])):
            position = content.index(image["placeholder"])
            if position < cursor:
                raise ValueError("prepared_prompt_image_invalid")
            if position > cursor:
                parts.append({"type": text_type, "text": content[cursor:position]})
            data_url = f"data:{image['contentType']};base64,{image['dataBase64']}"
            if api == "openai_responses":
                parts.append({"type": "input_image", "image_url": data_url})
            elif api == "anthropic_messages":
                parts.append({"type": "image", "source": {"type": "base64", "media_type": image["contentType"], "data": image["dataBase64"]}})
            elif api == "openai_completions":
                parts.append({"type": "image_url", "image_url": {"url": data_url}})
            else:
                raise ValueError("provider_api_unsupported")
            cursor = position + len(image["placeholder"])
        if cursor < len(content):
            parts.append({"type": text_type, "text": content[cursor:]})
        messages[message_indexes[message["messageId"]]]["content"] = parts
    return messages
