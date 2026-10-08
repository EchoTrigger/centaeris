import asyncio
import json

from asgiref.sync import sync_to_async
from django.conf import settings
from openai import AsyncOpenAI, OpenAI

from .common import (
    ModelConfig,
    ModelProviderError,
    build_messages,
    build_tool_choice,
    build_tools,
    model_payload,
    parse_tool_calls,
    provider_error,
    provider_client_options,
    request_thinking_mode,
    resolve_model_route,
    resolve_model_secret,
    validate_prepared_prompt,
    validated_usage,
)

GEMINI_TOOL_CONTINUATION_SCHEMA = "gemini_chat_tool_signatures.v1"
GEMINI_IMPORTED_TOOL_SIGNATURE = "skip_thought_signature_validator"


def _is_gemini(model: ModelConfig | None) -> bool:
    return getattr(getattr(model, "provider", None), "template_id", None) == "google"


def _gemini_tool_continuation(tool_calls: list[dict]) -> str:
    signatures = {}
    for call in tool_calls:
        extra = call.get("extra_content") or {}
        google = extra.get("google") if isinstance(extra, dict) else None
        signature = google.get("thought_signature") if isinstance(google, dict) else None
        if isinstance(signature, str) and signature:
            signatures[call["id"]] = signature
    if not tool_calls or tool_calls[0].get("id") not in signatures:
        raise ModelProviderError("provider_gemini_thought_signature_missing")
    return json.dumps(
        {"schema": GEMINI_TOOL_CONTINUATION_SCHEMA, "toolSignatures": signatures},
        separators=(",", ":"),
    )


def _project_gemini_messages(messages: list[dict]) -> None:
    for message in messages:
        if message["role"] != "assistant":
            continue
        raw = message.pop("reasoning_content", None)
        signatures = {}
        if isinstance(raw, str):
            try:
                continuation = json.loads(raw)
            except ValueError:
                continuation = None
            if (
                isinstance(continuation, dict)
                and continuation.get("schema") == GEMINI_TOOL_CONTINUATION_SCHEMA
            ):
                signatures = continuation.get("toolSignatures") or {}
        for index, call in enumerate(message.get("tool_calls") or []):
            signature = signatures.get(call["id"]) if isinstance(signatures, dict) else None
            if not isinstance(signature, str) or not signature:
                signature = None
            if signature is None and index == 0:
                signature = GEMINI_IMPORTED_TOOL_SIGNATURE
            if signature:
                call["extra_content"] = {"google": {"thought_signature": signature}}


def open_ai_completions_client(model: ModelConfig, request_body: dict | None = None) -> OpenAI:
    _, api_base = resolve_model_route(model)
    return OpenAI(
        api_key=resolve_model_secret(model),
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **provider_client_options(model, request_body),
    )


async def async_open_ai_completions_client(model: ModelConfig, request_body: dict | None = None) -> AsyncOpenAI:
    _, api_base = await sync_to_async(resolve_model_route, thread_sensitive=True)(model)
    api_key = await sync_to_async(resolve_model_secret, thread_sensitive=True)(model)
    options = await sync_to_async(provider_client_options, thread_sensitive=True)(model, request_body)
    return AsyncOpenAI(
        api_key=api_key,
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **options,
    )


def build_open_ai_completions_request(model: ModelConfig, request_body: dict) -> dict:
    prepared_prompt = validate_prepared_prompt(model, request_body)
    messages = build_messages(prepared_prompt)
    if _is_gemini(model):
        _project_gemini_messages(messages)
    payload = {
        "model": model.modelName,
        "messages": messages,
        "max_tokens": prepared_prompt["maxOutputTokens"],
    }
    thinking_mode = request_thinking_mode(model, request_body)
    if thinking_mode is not None:
        payload["reasoning_effort"] = thinking_mode
    tools = build_tools(prepared_prompt.get("toolDefinitions", []))
    if tools:
        payload["tools"] = tools
        payload["tool_choice"] = build_tool_choice(prepared_prompt.get("toolChoice"))
    return payload


def call_open_ai_completions(model: ModelConfig, request_body: dict) -> dict:
    client = open_ai_completions_client(model, request_body)
    try:
        return parse_open_ai_completions_response(
            model_payload(
                client.chat.completions.create(
                    **build_open_ai_completions_request(model, request_body)
                )
            ),
            gemini=_is_gemini(model),
        )
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        client.close()


async def stream_open_ai_completions(
    model: ModelConfig,
    request_body: dict,
    result_holder: dict,
    encode_event,
):
    text_parts = []
    reasoning_parts = []
    reasoning_field = None
    tool_calls = {}
    usage = {}
    saw_finish = False
    gemini = await sync_to_async(_is_gemini, thread_sensitive=True)(model)
    client = await async_open_ai_completions_client(model, request_body)
    try:
        request = build_open_ai_completions_request(model, request_body) | {"stream": True}
        request["stream_options"] = {"include_usage": True}
        stream = await client.chat.completions.create(**request)
        async for raw_chunk in stream:
            chunk = model_payload(raw_chunk)
            if isinstance(chunk.get("usage"), dict):
                usage = chunk["usage"]
            choices = chunk.get("choices") or []
            if not choices:
                continue
            choice = choices[0]
            saw_finish |= choice.get("finish_reason") is not None
            delta = choice.get("delta") or {}
            chunk_reasoning_field, chunk_reasoning = _openai_reasoning(delta)
            if chunk_reasoning_field is not None:
                if reasoning_field is not None and reasoning_field != chunk_reasoning_field:
                    raise ModelProviderError("provider_response_invalid")
                reasoning_field = chunk_reasoning_field
                reasoning_parts.append(chunk_reasoning)
                if chunk_reasoning:
                    yield encode_event("reasoning", {"text": "".join(reasoning_parts)})
            content = delta.get("content")
            if content:
                if not isinstance(content, str):
                    raise ModelProviderError("provider_response_invalid")
                text_parts.append(content)
                yield encode_event("delta", {"delta": content})
            elif content is not None and not isinstance(content, str):
                raise ModelProviderError("provider_response_invalid")
            for call in delta.get("tool_calls") or []:
                index = call.get("index")
                if not isinstance(index, int):
                    raise ModelProviderError("provider_response_invalid")
                aggregate = tool_calls.setdefault(index, {"id": "", "name": "", "argsJson": ""})
                if call.get("id"):
                    aggregate["id"] = call["id"]
                function = call.get("function") or {}
                if function.get("name"):
                    aggregate["name"] += function["name"]
                if function.get("arguments"):
                    aggregate["argsJson"] += function["arguments"]
                if call.get("extra_content") is not None:
                    aggregate["extra_content"] = call["extra_content"]
    except asyncio.CancelledError:
        raise
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        await client.close()
    if not saw_finish:
        raise ModelProviderError("provider_stream_interrupted")
    parsed_calls = []
    raw_calls = []
    for index in sorted(tool_calls):
        call = tool_calls[index]
        if not call["id"] or not call["name"]:
            raise ModelProviderError("provider_response_invalid")
        try:
            json.loads(call["argsJson"])
        except (TypeError, ValueError) as error:
            raise ModelProviderError("provider_response_invalid") from error
        raw_calls.append(call)
        parsed_calls.append({key: call[key] for key in ("id", "name", "argsJson")})
    gemini_continuation = _gemini_tool_continuation(raw_calls) if gemini and raw_calls else None
    result_holder["result"] = {
        "text": "".join(text_parts),
        "reasoningContent": "".join(reasoning_parts) if reasoning_field is not None else None,
        "continuationReasoningContent": gemini_continuation or ("".join(reasoning_parts) if reasoning_field is not None else None),
        "toolCalls": parsed_calls,
        "usage": validated_usage(usage),
    }


def parse_open_ai_completions_response(payload: dict, *, gemini: bool = False) -> dict:
    choice = (payload.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    text = message.get("content") or ""
    if not isinstance(text, str):
        raise ModelProviderError("provider_response_invalid")
    _, reasoning_content = _openai_reasoning(message)
    raw_calls = message.get("tool_calls") or []
    gemini_continuation = _gemini_tool_continuation(raw_calls) if gemini and raw_calls else None
    return {
        "text": text,
        "reasoningContent": reasoning_content,
        "continuationReasoningContent": gemini_continuation or reasoning_content,
        "toolCalls": parse_tool_calls(raw_calls),
        "usage": validated_usage(payload.get("usage")),
    }


def _openai_reasoning(payload: dict) -> tuple[str | None, str | None]:
    values = [
        (field, payload[field])
        for field in ("reasoning", "reasoning_content")
        if field in payload and payload[field] is not None
    ]
    if len(values) > 1:
        raise ModelProviderError("provider_response_invalid")
    if not values:
        return None, None
    field, value = values[0]
    if not isinstance(value, str):
        raise ModelProviderError("provider_response_invalid")
    return field, value
