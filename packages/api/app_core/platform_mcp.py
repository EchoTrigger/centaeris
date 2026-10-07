"""First-party, stateless material MCP transport; no plugin or runtime semantics."""

import json
from contextlib import asynccontextmanager
from contextvars import ContextVar

from asgiref.sync import sync_to_async
from django.conf import settings
from django.db import connections
from jsonschema import Draft202012Validator, ValidationError
from mcp import types
from mcp.server import Server
from mcp.server.transport_security import TransportSecuritySettings
from starlette.applications import Starlette
from starlette.responses import JSONResponse
from starlette.routing import Mount

from .material_access import bind_inputs, declared_inputs_for_run
from .material_contract import KnowledgeError
from .material_identity import representation_id
from .material_reads import read_materials, search_materials
from .material_delivery import save_result, read_result
from .material_operations import create_operations, operation_result
from .material_receipts import authorize_call, persist_receipt
from .platform_mcp_auth import CredentialRejected, authorize_context, verify_credential


request_context = ContextVar("platform_mcp_context", default=None)
request_call_id = ContextVar("platform_mcp_call_id", default=None)


def _database_call(function, *args):
    """MCP bypasses Django's request signals; own the connection cleanup here.

    Respect a caller-owned transaction (for embedded calls) rather than closing
    its connection. HTTP service calls run outside an atomic block.
    """
    def clean_connections():
        for connection in connections.all(initialized_only=True):
            if not connection.in_atomic_block:
                connection.close_if_unusable_or_obsolete()

    clean_connections()
    try:
        return function(*args)
    finally:
        clean_connections()


def _schema(properties, required=()):
    return {"type": "object", "properties": properties, "required": list(required), "additionalProperties": False}


TOOLS = (
    types.Tool(name="list_materials", description="List materials explicitly attached and authorized for this run; not the user's entire library.", inputSchema=_schema({})),
    types.Tool(name="read_material", description="Read an authorized material. Missing representations create durable processing operations; use get_operation for their status, then read again.", inputSchema=_schema({
        "input_ref": {"type": "string", "minLength": 1},
        "offset": {"type": "integer", "minimum": 0},
        "limit": {"type": "integer", "minimum": 1, "maximum": 2000},
    }, ("input_ref",))),
    types.Tool(name="search_materials", description="Keyword search this run's authorized materials. Missing representations create durable processing operations; query their status, then search again.", inputSchema=_schema({
        "query": {"type": "string", "minLength": 1},
        "input_refs": {"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": True, "maxItems": 128},
        "ranking": {"enum": ["relevance", "recent"]},
        "limit": {"type": "integer", "minimum": 1, "maximum": 20},
    }, ("query",))),
    types.Tool(name="read_material_result", description="Continue an immutable saved material result using its result_ref and cursor. Returns exact ranges and the next call; only returned evidence has been read.", inputSchema=_schema({
        "result_ref": {"type": "string", "minLength": 1, "maxLength": 96},
        "cursor": {"type": "string", "pattern": "^[0-9]{1,10}:[0-9]{1,12}$"},
    }, ("result_ref", "cursor"))),
    types.Tool(name="get_operation", description="Get the durable status of a material operation owned by this run. Completed means read/search can be retried.", inputSchema=_schema({"operation_id": {"type": "string", "minLength": 1, "maxLength": 96}}, ("operation_id",))),
    types.Tool(name="cancel_operation", description="Cancel this run's material operation. This detaches its wait and does not stop shared background processing or delete its result.", inputSchema=_schema({"operation_id": {"type": "string", "minLength": 1, "maxLength": 96}}, ("operation_id",))),
)
VALIDATORS = {tool.name: Draft202012Validator(tool.input_schema) for tool in TOOLS}


def execute_tool(context, name, arguments):
    access = authorize_context(context)
    call_id = request_call_id.get()
    call = authorize_call(access, call_id, name, arguments) if call_id is not None else None
    if name in {"get_operation", "cancel_operation"}:
        return operation_result(access, arguments["operation_id"], cancel=name == "cancel_operation")
    if name == "read_material_result":
        result = read_result(access, arguments["result_ref"], arguments["cursor"])
        return persist_receipt(access, call, name, result) if call is not None else result
    declared = declared_inputs_for_run(access.agent_run)
    refs = [arguments["input_ref"]] if name == "read_material" else arguments.get("input_refs", list(declared))
    if any(ref not in declared for ref in refs):
        raise KnowledgeError("knowledge_input_not_authorized", 403)
    bindings = [{"inputRef": ref, "representationId": representation_id(declared[ref]["inputIdentity"], access.spec_digest)} for ref in refs]
    if name == "list_materials":
        # Revalidate current source permissions and generations, not just the snapshot.
        bind_inputs(access.agent_run, access.authorization_digest, bindings, access.spec_digest)
        return {"items": [{"inputRef": ref, "displayName": declared[ref]["displayName"], "contentType": declared[ref]["contentType"], "sizeBytes": declared[ref]["sizeBytes"]} for ref in refs]}
    if name == "read_material":
        result = read_materials(access, bindings, offset=arguments.get("offset"), limit=arguments.get("limit"), full_window=True)
    elif name == "search_materials":
        result = search_materials(access, bindings, query=arguments["query"], ranking=arguments.get("ranking", "relevance"), date_range=None, limit=arguments.get("limit", 10), full_window=True)
    else:
        raise ValueError("unknown_tool")
    if result.get("disposition") == "pending":
        result["operations"] = create_operations(access, result["missing"])
    if call is not None and result.get("disposition") == "ready":
        result = save_result(access, call, result)
    if call is not None:
        result = persist_receipt(access, call, name, result)
    return result


async def _list_tools(context, params):
    return types.ListToolsResult(tools=list(TOOLS))


async def _call_tool(context, params):
    try:
        validator = VALIDATORS.get(params.name)
        if validator is None:
            raise ValueError("unknown_tool")
        arguments = params.arguments if params.arguments is not None else {}
        validator.validate(arguments)
        trusted = request_context.get()
        if trusted is None:
            raise CredentialRejected()
        result = await sync_to_async(_database_call, thread_sensitive=True)(execute_tool, trusted, params.name, arguments)
        return types.CallToolResult(content=[], structuredContent=result)
    except (ValidationError, ValueError, KnowledgeError) as error:
        # Never serialize validator instances, token claims, paths or model values.
        code = error.code if isinstance(error, KnowledgeError) else "platform_mcp_tool_rejected"
        return types.CallToolResult(isError=True, content=[types.TextContent(type="text", text=code)])


class ScopedAuthentication:
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.app(scope, receive, send)
        headers = scope.get("headers", [])
        tokens = [value for key, value in headers if key.lower() == b"authorization"]
        call_ids = [value for key, value in headers if key.lower() == b"x-workspace-tool-call-id"]
        try:
            if len(call_ids) > 1 or (call_ids and not 0 < len(call_ids[0]) <= 160):
                raise CredentialRejected()
            call_id = call_ids[0].decode("ascii") if call_ids else None
            if len(tokens) != 1 or any(key.lower() == b"origin" for key, _ in headers) or not tokens[0].startswith(b"Bearer "):
                raise CredentialRejected()
            context = await sync_to_async(_database_call, thread_sensitive=True)(verify_credential, tokens[0][7:].decode("ascii"))
        except (CredentialRejected, UnicodeDecodeError):
            return await JSONResponse({"error": "unauthorized"}, status_code=401, headers={"Cache-Control": "no-store", "WWW-Authenticate": "Bearer"})(scope, receive, send)
        marker = request_context.set(context)
        call_marker = request_call_id.set(call_id)
        async def send_private(message):
            if message["type"] == "http.response.start":
                message = {**message, "headers": [(key, value) for key, value in message.get("headers", []) if key.lower() != b"cache-control"] + [(b"cache-control", b"no-store")]}
            await send(message)
        try:
            await self.app(scope, receive, send_private)
        finally:
            request_call_id.reset(call_marker)
            request_context.reset(marker)


def create_mcp_app():
    server = Server("centaeris-workspace", version="0.1.0", on_list_tools=_list_tools, on_call_tool=_call_tool)
    # Separate explicit allowlist: never inherit Django's possible wildcard.
    hosts = getattr(settings, "PLATFORM_MCP_ALLOWED_HOSTS", ["localhost", "127.0.0.1"])
    if not hosts or any(not host or "*" in host or "/" in host for host in hosts):
        raise ValueError("PLATFORM_MCP_ALLOWED_HOSTS requires exact hostnames")
    transport = server.streamable_http_app(
        streamable_http_path="/internal/mcp", stateless_http=True, json_response=True,
        max_request_body_size=64 * 1024,
        transport_security=TransportSecuritySettings(allowed_hosts=[value for host in hosts for value in (host, f"{host}:*")], allowed_origins=[]),
    )

    @asynccontextmanager
    async def lifespan(app):
        async with transport.router.lifespan_context(transport):
            yield

    return Starlette(routes=[Mount("/", app=ScopedAuthentication(transport))], lifespan=lifespan)
