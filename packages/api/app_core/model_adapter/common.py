import asyncio
import json
import logging
import re
import httpx
from .images import project_images

from asgiref.sync import sync_to_async
from anthropic import (
    APIConnectionError as AnthropicAPIConnectionError,
    APITimeoutError as AnthropicAPITimeoutError,
)
from openai import (
    APIConnectionError,
    APIStatusError,
    APITimeoutError,
    AuthenticationError,
    RateLimitError,
)

from ..credentials import CredentialDecryptionError, decrypt_credential_secret
from ..models import (
    AgentRun,
    MODEL_API_IDS,
    ModelConfig,
    ModelEndpointValidationError,
    ModelProvider,
    ModelRunLog,
    ProviderCredential,
    validate_thinking_mode,
    validate_model_endpoint,
)


MODEL_STREAM_SCHEMA = "api.model.stream.v1"
PREPARED_PROMPT_FIELDS = {"schema", "systemPrompt", "messages", "toolDefinitions", "toolChoice", "maxOutputTokens", "inputImages"}
logger = logging.getLogger(__name__)


class ModelProviderError(RuntimeError):
    def __init__(self, reason_type: str, http_status: int | None = None, retry_after: str | None = None):
        super().__init__(reason_type)
        self.reasonType = reason_type
        self.httpStatus = http_status
        self.retryAfter = retry_after


def safe_model_error_reason(error: Exception) -> str:
    return error.reasonType if isinstance(error, ModelProviderError) else "model_adapter_failed"


def model_payload(value) -> dict:
    model_dump = getattr(value, "model_dump", None)
    payload = model_dump() if callable(model_dump) else dict(value) if isinstance(value, dict) else None
    if not isinstance(payload, dict):
        raise RuntimeError("model provider response payload is invalid")
    return payload


def provider_error(error: Exception) -> ModelProviderError | None:
    if isinstance(error, ModelProviderError):
        return None
    if isinstance(error, httpx.TimeoutException):
        return ModelProviderError("provider_timeout")
    if isinstance(error, httpx.NetworkError):
        return ModelProviderError("provider_unreachable")
    if isinstance(error, httpx.RemoteProtocolError):
        return ModelProviderError("provider_stream_interrupted")
    response = getattr(error, "response", None)
    headers = getattr(response, "headers", None)
    retry_after = headers.get("retry-after") if headers is not None else None
    if isinstance(error, AuthenticationError):
        return ModelProviderError("provider_authentication_failed", getattr(error, "status_code", 401))
    if isinstance(error, RateLimitError):
        return ModelProviderError("provider_rate_limited", getattr(error, "status_code", 429), retry_after)
    if isinstance(error, (APITimeoutError, AnthropicAPITimeoutError)):
        return ModelProviderError("provider_timeout")
    if isinstance(error, (APIConnectionError, AnthropicAPIConnectionError)):
        return ModelProviderError("provider_unreachable")
    status_code = getattr(error, "status_code", None)
    if status_code in {401, 403}:
        return ModelProviderError("provider_authentication_failed", status_code)
    if status_code == 429:
        return ModelProviderError("provider_rate_limited", status_code, retry_after)
    if status_code == 408:
        return ModelProviderError("provider_timeout", status_code, retry_after)
    if isinstance(error, APIStatusError):
        return ModelProviderError(
            "provider_unavailable" if error.status_code >= 500 else "provider_request_rejected",
            error.status_code,
            retry_after,
        )
    if isinstance(status_code, int):
        return ModelProviderError(
            "provider_unavailable" if status_code >= 500 else "provider_request_rejected",
            status_code,
            retry_after,
        )
    return None


def validated_usage(raw_usage) -> dict:
    required = {"prompt_tokens", "completion_tokens", "total_tokens"}
    if not isinstance(raw_usage, dict) or not required.issubset(raw_usage):
        raise ModelProviderError("provider_usage_invalid")
    if any(not isinstance(raw_usage[key], int) or raw_usage[key] < 0 for key in required):
        raise ModelProviderError("provider_usage_invalid")
    prompt_tokens = raw_usage["prompt_tokens"]
    hit_tokens = raw_usage.get("prompt_cache_hit_tokens")
    if hit_tokens is None:
        details = raw_usage.get("prompt_tokens_details")
        hit_tokens = details.get("cached_tokens") if isinstance(details, dict) else None
    if hit_tokens is None:
        hit_tokens = raw_usage.get("cache_read_input_tokens")
    miss_tokens = raw_usage.get("prompt_cache_miss_tokens")
    if hit_tokens is not None and miss_tokens is None:
        miss_tokens = prompt_tokens - hit_tokens
    if any(
        value is not None and (not isinstance(value, int) or value < 0)
        for value in (hit_tokens, miss_tokens)
    ):
        raise ModelProviderError("provider_usage_invalid")
    if hit_tokens is not None and miss_tokens is not None and hit_tokens + miss_tokens != prompt_tokens:
        raise ModelProviderError("provider_usage_invalid")
    return {
        "prompt_tokens": prompt_tokens,
        "completion_tokens": raw_usage["completion_tokens"],
        "total_tokens": raw_usage["total_tokens"],
        "prompt_cache_hit_tokens": hit_tokens,
        "prompt_cache_miss_tokens": miss_tokens,
    }


def credential_for_provider(provider: ModelProvider, lock: bool = False) -> ProviderCredential:
    query = ProviderCredential.objects
    if lock:
        query = query.select_for_update()
    try:
        return query.get(provider_id=provider.id)
    except ProviderCredential.DoesNotExist as error:
        raise ModelProviderError("provider_secret_unavailable") from error


def credential_for_model(model: ModelConfig, lock: bool = False) -> ProviderCredential:
    if model.provider_id is None:
        raise ModelProviderError("provider_secret_unavailable")
    try:
        provider = ModelProvider.objects.get(id=model.provider_id)
    except ModelProvider.DoesNotExist as error:
        raise ModelProviderError("provider_unavailable") from error
    return credential_for_provider(provider, lock=lock)


def resolve_model_secret(model: ModelConfig) -> str:
    credential = credential_for_model(model)
    try:
        return decrypt_credential_secret(credential.encryptedSecret)
    except CredentialDecryptionError as error:
        raise ModelProviderError("provider_secret_decryption_failed") from error


def resolve_model_route(model: ModelConfig) -> tuple[str, str]:
    if model.provider_id is None:
        raise ModelProviderError("provider_unsupported")
    try:
        provider = ModelProvider.objects.get(id=model.provider_id)
    except ModelProvider.DoesNotExist as error:
        raise ModelProviderError("provider_unavailable") from error
    if not provider.enabled or provider.archivedAt is not None:
        raise ModelProviderError("provider_unavailable")
    if model.resolvedApi not in MODEL_API_IDS:
        raise ModelProviderError("provider_api_unsupported")
    try:
        validate_model_endpoint(model.resolvedApiBase)
    except ModelEndpointValidationError as error:
        raise ModelProviderError(error.code) from error
    return model.resolvedApi, model.resolvedApiBase


def provider_client_options(model: ModelConfig, request_body: dict | None = None) -> dict:
    if getattr(getattr(model, "provider", None), "template_id", None) != "opencode_zen_go":
        return {}
    run_id = (request_body or {}).get("agentRunId")
    if run_id:
        try:
            session_id = AgentRun.objects.get(id=run_id, modelConfig_id=model.id).session_id
        except AgentRun.DoesNotExist as error:
            raise ModelProviderError("model_session_unavailable") from error
    else:
        session_id = f"model-test:{model.id}"
    return {"default_headers": {
        "User-Agent": "CentaerisWorkspace",
        "x-opencode-session": session_id,
    }}


def validate_prepared_prompt(model: ModelConfig, request_body: dict) -> dict:
    prepared_prompt = request_body.get("preparedPrompt")
    if not isinstance(prepared_prompt, dict) or prepared_prompt.get("schema") != "prepared_prompt.v1":
        raise ModelProviderError("prepared_prompt_invalid")
    if set(prepared_prompt) - PREPARED_PROMPT_FIELDS:
        raise ModelProviderError("prepared_prompt_fields_invalid")
    max_output_tokens = prepared_prompt.get("maxOutputTokens")
    if (
        not isinstance(max_output_tokens, int)
        or isinstance(max_output_tokens, bool)
        or max_output_tokens <= 0
        or max_output_tokens > model.maxOutputTokens
    ):
        raise ModelProviderError("prepared_prompt_output_limit_mismatch")
    build_messages(prepared_prompt)
    tools = build_tools(prepared_prompt.get("toolDefinitions"))
    tool_choice = prepared_prompt.get("toolChoice")
    if tools:
        build_tool_choice(tool_choice)
        if tool_choice.get("type") == "specific":
            projected_names = {tool["function"]["name"] for tool in tools}
            if tool_choice.get("name") not in projected_names:
                raise ModelProviderError("prepared_prompt_tool_choice_invalid")
    elif tool_choice != {"type": "none"}:
        raise ModelProviderError("prepared_prompt_tool_choice_invalid")
    return prepared_prompt


def request_thinking_mode(model: ModelConfig, request_body: dict) -> str | None:
    value = request_body.get("thinkingMode")
    if value is None:
        return None
    try:
        validate_thinking_mode(value)
    except ValueError as error:
        raise ModelProviderError("model_thinking_mode_unsupported") from error
    if value not in model.thinkingModes:
        raise ModelProviderError("model_thinking_mode_unsupported")
    return value


def build_messages(prepared_prompt: dict, image_api: str = "openai_completions") -> list[dict]:
    messages = []
    system_prompt = prepared_prompt.get("systemPrompt")
    if system_prompt is not None and (
        not isinstance(system_prompt, str) or not system_prompt.strip()
    ):
        raise ModelProviderError("prepared_prompt_system_prompt_invalid")
    if isinstance(system_prompt, str) and system_prompt.strip():
        messages.append({"role": "system", "content": system_prompt})
    raw_messages = prepared_prompt.get("messages")
    if not isinstance(raw_messages, list):
        raise ModelProviderError("prepared_prompt_messages_invalid")
    pending_tool_call_ids = []
    seen_message_ids = set()
    seen_tool_call_ids = set()
    message_indexes = {}
    for message in raw_messages:
        projected_index = len(messages)
        if not isinstance(message, dict):
            raise ModelProviderError("prepared_prompt_message_invalid")
        message_id = message.get("messageId")
        if not isinstance(message_id, str) or not message_id.strip():
            raise ModelProviderError("prepared_prompt_message_id_invalid")
        if message_id in seen_message_ids:
            raise ModelProviderError("prepared_prompt_message_id_duplicate")
        seen_message_ids.add(message_id)
        role = message.get("role")
        if role not in {"system", "user", "assistant", "tool"}:
            raise ModelProviderError("prepared_prompt_message_role_invalid")
        allowed_fields = {"messageId", "role", "content"}
        if role == "assistant":
            allowed_fields |= {"toolCalls", "reasoningContent"}
        elif role == "tool":
            allowed_fields.add("toolCallId")
        if set(message) - allowed_fields:
            raise ModelProviderError("prepared_prompt_message_fields_invalid")
        if pending_tool_call_ids and role != "tool":
            raise ModelProviderError("prepared_prompt_tool_pairing_invalid")
        content = message.get("content", "")
        if not isinstance(content, str):
            raise ModelProviderError("prepared_prompt_message_content_invalid")
        if role == "assistant":
            item = {"role": "assistant", "content": content}
            tool_calls = assistant_tool_calls(message)
            if tool_calls:
                item["tool_calls"] = tool_calls
                for call in tool_calls:
                    if call["id"] in seen_tool_call_ids:
                        raise ModelProviderError("prepared_prompt_tool_call_id_duplicate")
                    seen_tool_call_ids.add(call["id"])
                    pending_tool_call_ids.append(call["id"])
            reasoning_content = message.get("reasoningContent")
            if reasoning_content is not None and not isinstance(reasoning_content, str):
                raise ModelProviderError("prepared_prompt_reasoning_content_invalid")
            if isinstance(reasoning_content, str):
                item["reasoning_content"] = reasoning_content
            messages.append(item)
        elif role == "tool":
            tool_call_id = message.get("toolCallId")
            if not isinstance(tool_call_id, str) or not tool_call_id:
                raise ModelProviderError("prepared_prompt_tool_result_missing_call_id")
            if not pending_tool_call_ids or pending_tool_call_ids[0] != tool_call_id:
                raise ModelProviderError("prepared_prompt_tool_pairing_invalid")
            pending_tool_call_ids.pop(0)
            messages.append({"role": "tool", "tool_call_id": tool_call_id, "content": content})
        elif role in {"system", "user"} and content.strip():
            messages.append({"role": role, "content": content})
        if len(messages) > projected_index:
            message_indexes[message_id] = projected_index
    if pending_tool_call_ids:
        raise ModelProviderError("prepared_prompt_tool_pairing_invalid")
    if not messages:
        raise ModelProviderError("prepared_prompt_messages_required")
    try:
        return project_images(prepared_prompt, messages, message_indexes, image_api)
    except ValueError as error:
        raise ModelProviderError(str(error)) from error


def assistant_tool_calls(message: dict) -> list[dict]:
    raw_calls = message.get("toolCalls", [])
    if not isinstance(raw_calls, list):
        raise ModelProviderError("prepared_prompt_tool_calls_invalid")
    calls = []
    for call in raw_calls:
        if not isinstance(call, dict) or set(call) != {"id", "name", "argsJson"}:
            raise ModelProviderError("prepared_prompt_tool_call_invalid")
        call_id = call.get("id")
        name = call.get("name")
        args_json = call.get("argsJson")
        if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name:
            raise ModelProviderError("prepared_prompt_tool_call_invalid")
        if not isinstance(args_json, str):
            raise ModelProviderError("prepared_prompt_tool_call_args_invalid")
        try:
            json.loads(args_json)
        except (TypeError, ValueError) as error:
            raise ModelProviderError("prepared_prompt_tool_call_args_invalid") from error
        calls.append(
            {
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": args_json},
            }
        )
    return calls


def build_tools(tool_definitions) -> list[dict]:
    if not isinstance(tool_definitions, list):
        raise ModelProviderError("prepared_prompt_tool_definitions_invalid")
    tools = []
    seen_names = set()
    for definition in tool_definitions:
        if not isinstance(definition, dict) or set(definition) != {
            "name",
            "description",
            "inputSchema",
        }:
            raise ModelProviderError("prepared_prompt_tool_definition_invalid")
        name = definition.get("name")
        description = definition.get("description")
        input_schema = definition.get("inputSchema")
        if not isinstance(name, str) or not name or not isinstance(description, str):
            raise ModelProviderError("prepared_prompt_tool_definition_invalid")
        if not isinstance(input_schema, dict) or name in seen_names:
            raise ModelProviderError("prepared_prompt_tool_definition_schema_invalid")
        seen_names.add(name)
        tools.append(
            {
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": input_schema,
                },
            }
        )
    return tools


def build_tool_choice(tool_choice) -> str | dict:
    if not isinstance(tool_choice, dict):
        raise ModelProviderError("prepared_prompt_tool_choice_invalid")
    kind = tool_choice.get("type")
    if kind in {"auto", "none", "required"} and set(tool_choice) == {"type"}:
        return kind
    if kind == "specific" and set(tool_choice) == {"type", "name"}:
        name = tool_choice.get("name")
        if isinstance(name, str) and name:
            return {"type": "function", "function": {"name": name}}
    raise ModelProviderError("prepared_prompt_tool_choice_invalid")


def parse_tool_calls(raw_tool_calls) -> list[dict]:
    if not raw_tool_calls:
        return []
    if not isinstance(raw_tool_calls, list):
        raise RuntimeError("model provider tool_calls must be a list")
    calls = []
    for index, item in enumerate(raw_tool_calls):
        function = item.get("function", {}) if isinstance(item, dict) else {}
        name = function.get("name")
        arguments = function.get("arguments", "{}")
        if not isinstance(name, str) or not name or not isinstance(arguments, str):
            raise RuntimeError("model provider tool call is invalid")
        try:
            json.loads(arguments or "{}")
        except ValueError as error:
            raise RuntimeError("model provider tool call arguments are invalid") from error
        calls.append(
            {
                "id": str(item.get("id") or f"call_{index}"),
                "name": name,
                "argsJson": arguments if arguments.strip() else "{}",
            }
        )
    return calls


def fake_model_response(model: ModelConfig, request_body: dict) -> dict:
    prepared_prompt = validate_prepared_prompt(model, request_body)
    messages = prepared_prompt["messages"]
    if model.modelName == "fake-streaming":
        return {"text": "流式输出用于验证断线续传。" * 20, "toolCalls": [], "usage": zero_usage()}
    if model.modelName in {"fake-evidence", "fake-evidence-pdf"}:
        query = "preop" if model.modelName == "fake-evidence-pdf" else "术前"
        agent_run_id = str(request_body.get("agentRunId", "")).strip()
        completed_calls = ModelRunLog.objects.filter(agentRunId=agent_run_id, status="success").count() if agent_run_id else 0
        tool_results = [message.get("content", "") for message in messages if message.get("role") == "tool"]
        if completed_calls == 0:
            return fake_tool_call("call_rga", "bash", {"command": f'rga -n "{query}" /mnt/data', "timeoutMs": 10000})
        latest = tool_results[-1] if tool_results else ""
        if completed_calls == 1 and "Bash ok" in latest:
            matched_path = re.search(r"/mnt/data/[^\s:]+\.(?:md|txt|pdf)", latest, flags=re.IGNORECASE)
            if matched_path is None:
                return {"text": "未在授权资料中找到可读取的依据。", "toolCalls": [], "usage": zero_usage()}
            return fake_tool_call("call_read", "read", {"path": matched_path.group(0), "limit": 50})
        return {"text": "根据授权资料，术前应完成身份确认和风险告知。", "toolCalls": [], "usage": zero_usage()}
    if model.modelName == "fake-bash":
        tool_results = [message.get("content", "") for message in messages if message.get("role") == "tool"]
        if not tool_results:
            return fake_tool_call("call_bash", "bash", {"command": "printf remote-bash-ok", "timeoutMs": 5000})
        text = "远程 Bash 执行成功。" if "remote-bash-ok" in tool_results[-1] else "Bash 结构化失败；未执行本地回退。"
        return {"text": text, "toolCalls": [], "usage": zero_usage()}
    if model.modelName == "fake-tool-call":
        if not any(message.get("role") == "tool" for message in messages):
            return fake_tool_call("call_write", "write", {"filename": "report.md", "content": "hello", "mimeType": "text/markdown"})
        return {"text": "已生成 report.md。", "toolCalls": [], "usage": zero_usage()}
    return {"text": "这是最小纵切响应。", "toolCalls": [], "usage": zero_usage()}


def zero_usage() -> dict:
    return {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}


def fake_tool_call(call_id: str, name: str, arguments: dict) -> dict:
    return {"text": "", "toolCalls": [{"id": call_id, "name": name, "argsJson": json.dumps(arguments)}], "usage": zero_usage()}


@sync_to_async(thread_sensitive=True)
def record_model_run(
    *,
    agent_run_id: str,
    model: ModelConfig,
    status: str,
    usage: dict | None = None,
    error: str = "",
) -> None:
    usage = usage or {}
    ModelRunLog.objects.create(
        agentRunId=agent_run_id,
        modelConfig=model,
        status=status,
        promptTokens=usage.get("prompt_tokens"),
        completionTokens=usage.get("completion_tokens"),
        totalTokens=usage.get("total_tokens"),
        promptCacheHitTokens=usage.get("prompt_cache_hit_tokens"),
        promptCacheMissTokens=usage.get("prompt_cache_miss_tokens"),
        error=error,
    )


async def record_model_run_cancellation_safe(**kwargs) -> None:
    record_task = asyncio.create_task(record_model_run(**kwargs))
    cancellation = None
    while not record_task.done():
        try:
            await asyncio.shield(record_task)
        except asyncio.CancelledError as error:
            cancellation = error
    record_task.result()
    if cancellation is not None:
        raise cancellation
