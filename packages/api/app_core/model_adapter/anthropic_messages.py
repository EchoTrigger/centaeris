import asyncio
import json

from anthropic import Anthropic, AsyncAnthropic
from asgiref.sync import sync_to_async
from django.conf import settings

from .common import (
    ModelConfig,
    ModelProviderError,
    build_messages,
    build_tool_choice,
    build_tools,
    model_payload,
    provider_error,
    provider_client_options,
    request_thinking_mode,
    resolve_model_route,
    resolve_model_secret,
    validate_prepared_prompt,
    validated_usage,
)


def anthropic_messages_client(model: ModelConfig, request_body: dict | None = None) -> Anthropic:
    _, api_base = resolve_model_route(model)
    return Anthropic(
        api_key=resolve_model_secret(model),
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **provider_client_options(model, request_body),
    )


async def async_anthropic_messages_client(model: ModelConfig, request_body: dict | None = None) -> AsyncAnthropic:
    _, api_base = await sync_to_async(resolve_model_route, thread_sensitive=True)(model)
    api_key = await sync_to_async(resolve_model_secret, thread_sensitive=True)(model)
    options = await sync_to_async(provider_client_options, thread_sensitive=True)(model, request_body)
    return AsyncAnthropic(
        api_key=api_key,
        base_url=api_base,
        timeout=settings.MODEL_PROVIDER_TIMEOUT_SECONDS,
        max_retries=0,
        **options,
    )


def build_anthropic_messages_request(model: ModelConfig, request_body: dict) -> dict:
    prepared_prompt = validate_prepared_prompt(model, request_body)
    messages = build_messages(prepared_prompt, "anthropic_messages")
    system = "\n\n".join(
        message["content"] for message in messages if message["role"] == "system"
    )
    payload = {
        "model": model.modelName,
        "max_tokens": prepared_prompt["maxOutputTokens"],
        "messages": anthropic_messages(messages),
    }
    thinking_mode = request_thinking_mode(model, request_body)
    if thinking_mode is not None:
        payload["output_config"] = {"effort": thinking_mode}
    if system:
        payload["system"] = system
    tools = build_tools(prepared_prompt.get("toolDefinitions", []))
    choice = build_tool_choice(prepared_prompt.get("toolChoice")) if tools else "none"
    if tools and choice != "none":
        payload["tools"] = [
            {
                "name": tool["function"]["name"],
                "description": tool["function"]["description"],
                "input_schema": tool["function"]["parameters"],
            }
            for tool in tools
        ]
        payload["tool_choice"] = anthropic_tool_choice(choice)
    return payload


def anthropic_messages(messages: list[dict]) -> list[dict]:
    projected = []
    for message in messages:
        role = message["role"]
        if role == "system":
            continue
        if role == "tool":
            projected.append(
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": message["tool_call_id"],
                            "content": message["content"],
                        }
                    ],
                }
            )
            continue
        if role == "assistant" and message.get("tool_calls"):
            content = []
            if message["content"]:
                content.append({"type": "text", "text": message["content"]})
            for call in message["tool_calls"]:
                content.append(
                    {
                        "type": "tool_use",
                        "id": call["id"],
                        "name": call["function"]["name"],
                        "input": json.loads(call["function"]["arguments"]),
                    }
                )
            projected.append({"role": "assistant", "content": content})
            continue
        projected.append({"role": role, "content": message["content"]})
    return projected


def anthropic_tool_choice(choice: str | dict) -> dict:
    if choice == "auto":
        return {"type": "auto"}
    if choice == "required":
        return {"type": "any"}
    if isinstance(choice, dict):
        return {"type": "tool", "name": choice["function"]["name"]}
    raise ModelProviderError("prepared_prompt_tool_choice_invalid")


def call_anthropic_messages(model: ModelConfig, request_body: dict) -> dict:
    client = anthropic_messages_client(model, request_body)
    try:
        return parse_anthropic_message(
            model_payload(client.messages.create(**build_anthropic_messages_request(model, request_body)))
        )
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        client.close()


async def stream_anthropic_messages(
    model: ModelConfig,
    request_body: dict,
    result_holder: dict,
    encode_event,
):
    text_parts = []
    reasoning_blocks = {}
    tool_calls = {}
    usage = {}
    saw_stop = False
    client = await async_anthropic_messages_client(model, request_body)
    try:
        stream = await client.messages.create(
            **(build_anthropic_messages_request(model, request_body) | {"stream": True})
        )
        async for raw_event in stream:
            event = model_payload(raw_event)
            event_type = event.get("type")
            if event_type == "message_start":
                message = event.get("message")
                if isinstance(message, dict) and isinstance(message.get("usage"), dict):
                    usage |= message["usage"]
            elif event_type == "message_delta":
                delta = event.get("usage")
                if isinstance(delta, dict):
                    usage |= delta
            elif event_type == "content_block_start":
                block = event.get("content_block")
                index = event.get("index")
                if isinstance(index, int) and isinstance(block, dict) and block.get("type") == "thinking":
                    reasoning_blocks[index] = thinking_text(block)
                    if reasoning_blocks[index]:
                        yield encode_event("reasoning", {"text": "\n".join(value for _, value in sorted(reasoning_blocks.items()) if value)})
                if isinstance(index, int) and isinstance(block, dict) and block.get("type") == "tool_use":
                    tool_calls[index] = {
                        "id": block.get("id"),
                        "name": block.get("name"),
                        "argsJson": (
                            ""
                            if block.get("input", {}) == {}
                            else json.dumps(block["input"])
                        ),
                    }
            elif event_type == "content_block_delta":
                index = event.get("index")
                delta = event.get("delta")
                if not isinstance(delta, dict):
                    continue
                if delta.get("type") == "text_delta":
                    text = delta.get("text")
                    if not isinstance(text, str):
                        raise RuntimeError("provider text delta is invalid")
                    text_parts.append(text)
                    yield encode_event("delta", {"delta": text})
                elif delta.get("type") == "thinking_delta":
                    if not isinstance(index, int) or index not in reasoning_blocks:
                        raise RuntimeError("provider thinking delta has no matching block")
                    reasoning_blocks[index] += thinking_text(delta)
                    yield encode_event("reasoning", {"text": "\n".join(value for _, value in sorted(reasoning_blocks.items()) if value)})
                elif delta.get("type") == "input_json_delta":
                    if not isinstance(index, int) or index not in tool_calls:
                        raise RuntimeError("provider tool call delta is invalid")
                    partial_json = delta.get("partial_json")
                    if not isinstance(partial_json, str):
                        raise RuntimeError("provider tool call delta is invalid")
                    tool_calls[index]["argsJson"] += partial_json
            elif event_type == "message_stop":
                saw_stop = True
    except asyncio.CancelledError:
        raise
    except Exception as error:
        mapped = provider_error(error)
        if mapped is not None:
            raise mapped from error
        raise
    finally:
        await client.close()
    if not saw_stop:
        raise ModelProviderError("provider_stream_interrupted")
    parsed_calls = []
    for index in sorted(tool_calls):
        call = tool_calls[index]
        if not isinstance(call["id"], str) or not call["id"] or not isinstance(call["name"], str) or not call["name"]:
            raise RuntimeError("provider tool call stream is incomplete")
        call["argsJson"] = call["argsJson"] or "{}"
        json.loads(call["argsJson"])
        parsed_calls.append(call)
    result_holder["result"] = {
        "text": "".join(text_parts),
        "reasoningContent": "\n".join(reasoning_blocks[index] for index in sorted(reasoning_blocks) if reasoning_blocks[index]) or None,
        "toolCalls": parsed_calls,
        "usage": anthropic_usage(usage),
    }


def parse_anthropic_message(payload: dict) -> dict:
    text_parts = []
    reasoning_parts = []
    tool_calls = []
    for block in payload.get("content") or []:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "text":
            text = block.get("text")
            if not isinstance(text, str):
                raise RuntimeError("provider response text is invalid")
            text_parts.append(text)
        elif block.get("type") == "thinking":
            text = thinking_text(block)
            if text:
                reasoning_parts.append(text)
        elif block.get("type") == "tool_use":
            call_id = block.get("id")
            name = block.get("name")
            arguments = block.get("input")
            if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name or not isinstance(arguments, dict):
                raise RuntimeError("provider tool call is invalid")
            tool_calls.append({"id": call_id, "name": name, "argsJson": json.dumps(arguments)})
    return {
        "text": "".join(text_parts),
        "reasoningContent": "\n".join(reasoning_parts) or None,
        "toolCalls": tool_calls,
        "usage": anthropic_usage(payload.get("usage")),
    }


def thinking_text(block: dict) -> str:
    text = block.get("thinking")
    if not isinstance(text, str):
        raise RuntimeError("provider thinking text is invalid")
    return text


def anthropic_usage(raw_usage) -> dict:
    if not isinstance(raw_usage, dict):
        raise ModelProviderError("provider_usage_invalid")
    input_tokens = raw_usage.get("input_tokens")
    output_tokens = raw_usage.get("output_tokens")
    cache_read = raw_usage.get("cache_read_input_tokens", 0)
    cache_write = raw_usage.get("cache_creation_input_tokens", 0)
    if any(not isinstance(value, int) or value < 0 for value in (input_tokens, output_tokens, cache_read, cache_write)):
        raise ModelProviderError("provider_usage_invalid")
    prompt_tokens = input_tokens + cache_read + cache_write
    return validated_usage(
        {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": prompt_tokens + output_tokens,
            "prompt_cache_hit_tokens": cache_read,
            "prompt_cache_miss_tokens": input_tokens + cache_write,
        }
    )
