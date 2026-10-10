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
    model_database_operation,
    provider_error,
    provider_client_options,
    request_thinking_mode,
    resolve_model_route,
    resolve_model_secret,
    validate_prepared_prompt,
    validated_usage,
)


@model_database_operation
def _client_arguments(model, request_body):
    _, api_base = resolve_model_route(model)
    return resolve_model_secret(model), api_base, provider_client_options(model, request_body)


def open_ai_responses_client(model: ModelConfig, request_body: dict | None = None) -> OpenAI:
    api_key, api_base, options = _client_arguments(model, request_body)
    return OpenAI(
        api_key=api_key,
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **options,
    )


async def async_open_ai_responses_client(model: ModelConfig, request_body: dict | None = None) -> AsyncOpenAI:
    api_key, api_base, options = await sync_to_async(_client_arguments, thread_sensitive=True)(model, request_body)
    return AsyncOpenAI(
        api_key=api_key,
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **options,
    )


def build_open_ai_responses_request(model: ModelConfig, request_body: dict) -> dict:
    prepared_prompt = validate_prepared_prompt(model, request_body)
    messages = build_messages(prepared_prompt, "openai_responses")
    instructions = "\n\n".join(
        message["content"] for message in messages if message["role"] == "system"
    )
    input_items = []
    for message in messages:
        role = message["role"]
        if role == "system":
            continue
        if role == "tool":
            input_items.append(
                {
                    "type": "function_call_output",
                    "call_id": message["tool_call_id"],
                    "output": message["content"],
                }
            )
            continue
        input_items.append({"role": role, "content": message["content"]})
        for call in message.get("tool_calls", []):
            input_items.append(
                {
                    "type": "function_call",
                    "call_id": call["id"],
                    "name": call["function"]["name"],
                    "arguments": call["function"]["arguments"],
                }
            )
    payload = {
        "model": model.modelName,
        "input": input_items,
        "max_output_tokens": prepared_prompt["maxOutputTokens"],
    }
    thinking_mode = request_thinking_mode(model, request_body)
    if thinking_mode is not None:
        payload["reasoning"] = {"effort": thinking_mode}
        if thinking_mode != "none":
            payload["reasoning"]["summary"] = "auto"
    if instructions:
        payload["instructions"] = instructions
    tools = build_tools(prepared_prompt.get("toolDefinitions", []))
    if tools:
        payload["tools"] = [
            {
                "type": "function",
                "name": tool["function"]["name"],
                "description": tool["function"]["description"],
                "parameters": tool["function"]["parameters"],
            }
            for tool in tools
        ]
        payload["tool_choice"] = response_tool_choice(
            build_tool_choice(prepared_prompt.get("toolChoice"))
        )
    return payload


def response_tool_choice(choice: str | dict) -> str | dict:
    if isinstance(choice, str):
        return choice
    return {"type": "function", "name": choice["function"]["name"]}


def call_open_ai_responses(model: ModelConfig, request_body: dict) -> dict:
    client = open_ai_responses_client(model, request_body)
    try:
        return parse_open_ai_responses_response(
            model_payload(client.responses.create(**build_open_ai_responses_request(model, request_body)))
        )
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        client.close()


async def stream_open_ai_responses(
    model: ModelConfig,
    request_body: dict,
    result_holder: dict,
    encode_event,
):
    text_parts = []
    tool_calls = []
    completed_response = None
    reasoning_parts = {}
    client = await async_open_ai_responses_client(model, request_body)
    try:
        stream = await client.responses.create(
            **(build_open_ai_responses_request(model, request_body) | {"stream": True})
        )
        async for raw_event in stream:
            event = model_payload(raw_event)
            event_type = event.get("type")
            if event_type in {"response.reasoning_summary_text.delta", "response.reasoning_summary_text.done"}:
                index = (event.get("output_index"), event.get("summary_index"))
                if any(type(value) is not int or value < 0 for value in index):
                    raise RuntimeError("provider reasoning summary indices are invalid")
                delta = event_type.endswith(".delta")
                text = event.get("delta" if delta else "text")
                if not isinstance(text, str):
                    raise RuntimeError("provider reasoning summary text is invalid")
                reasoning_parts[index] = reasoning_parts.get(index, "") + text if delta else text
                yield encode_event("reasoning", {"text": "\n".join(value for _, value in sorted(reasoning_parts.items()) if value)})
            if event_type == "response.output_text.delta":
                delta = event.get("delta")
                if not isinstance(delta, str):
                    raise RuntimeError("provider text delta is invalid")
                text_parts.append(delta)
                yield encode_event("delta", {"delta": delta})
            elif event_type == "response.output_item.done":
                item = event.get("item")
                if isinstance(item, dict) and item.get("type") == "function_call":
                    tool_calls.append(parse_response_tool_call(item))
            elif event_type == "response.completed":
                completed_response = event.get("response")
                text = parse_open_ai_responses_response(completed_response)["reasoningContent"]
                if text:
                    yield encode_event("reasoning", {"text": text})
    except asyncio.CancelledError:
        raise
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        await client.close()
    if not isinstance(completed_response, dict):
        raise ModelProviderError("provider_stream_interrupted")
    terminal = parse_open_ai_responses_response(completed_response)
    result_holder["result"] = {
        "text": "".join(text_parts) if text_parts else terminal["text"],
        "reasoningContent": terminal["reasoningContent"],
        "toolCalls": tool_calls or terminal["toolCalls"],
        "usage": terminal["usage"],
    }


def parse_open_ai_responses_response(payload: dict) -> dict:
    text_parts = []
    reasoning_parts = []
    tool_calls = []
    for item in payload.get("output") or []:
        if not isinstance(item, dict):
            continue
        if item.get("type") == "function_call":
            tool_calls.append(parse_response_tool_call(item))
        if item.get("type") == "reasoning":
            for summary in item.get("summary") or []:
                if not isinstance(summary, dict) or summary.get("type") != "summary_text":
                    continue
                text = summary.get("text")
                if not isinstance(text, str):
                    raise RuntimeError("provider reasoning summary text is invalid")
                if text:
                    reasoning_parts.append(text)
        if item.get("type") != "message":
            continue
        for content in item.get("content") or []:
            if isinstance(content, dict) and content.get("type") == "output_text":
                text = content.get("text")
                if not isinstance(text, str):
                    raise RuntimeError("provider response text is invalid")
                text_parts.append(text)
    output_text = payload.get("output_text")
    if not text_parts and isinstance(output_text, str):
        text_parts.append(output_text)
    return {
        "text": "".join(text_parts),
        "reasoningContent": "\n".join(reasoning_parts) or None,
        "toolCalls": tool_calls,
        "usage": response_usage(payload.get("usage")),
    }


def parse_response_tool_call(item: dict) -> dict:
    call_id = item.get("call_id")
    name = item.get("name")
    arguments = item.get("arguments")
    if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name:
        raise RuntimeError("provider response function call is invalid")
    if not isinstance(arguments, str):
        raise RuntimeError("provider response function call arguments are invalid")
    try:
        json.loads(arguments or "{}")
    except ValueError as error:
        raise RuntimeError("provider response function call arguments are invalid") from error
    return {"id": call_id, "name": name, "argsJson": arguments or "{}"}


def response_usage(raw_usage) -> dict:
    if not isinstance(raw_usage, dict):
        raise ModelProviderError("provider_usage_invalid")
    input_tokens = raw_usage.get("input_tokens")
    output_tokens = raw_usage.get("output_tokens")
    total_tokens = raw_usage.get("total_tokens")
    if not isinstance(input_tokens, int) or not isinstance(output_tokens, int):
        raise ModelProviderError("provider_usage_invalid")
    if not isinstance(total_tokens, int):
        total_tokens = input_tokens + output_tokens
    details = raw_usage.get("input_tokens_details")
    cached_tokens = details.get("cached_tokens") if isinstance(details, dict) else None
    return validated_usage(
        {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": total_tokens,
            "prompt_cache_hit_tokens": cached_tokens,
        }
    )
