import json


def decode_json_object(request) -> dict:
    if not request.body:
        return {}
    try:
        body = json.loads(request.body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("invalid_json") from error
    if not isinstance(body, dict):
        raise ValueError("invalid_json")
    return body
