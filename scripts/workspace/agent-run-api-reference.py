"""Generate the public Agent Run API reference bundled by Workspace Web."""

import argparse
import json
from pathlib import Path
import re

from django.conf import settings
from pydantic import TypeAdapter
from pydantic.alias_generators import to_camel


OUTPUT = Path(__file__).resolve().parents[2] / "packages/web/src/developer/agentRunApiReference.json"


def _public_path(path):
    return "/api" + re.sub(r"\{(\w+)\}", lambda match: "{" + to_camel(match[1]) + "}", path)


def _reference():
    # The wire types only need Ninja's default configuration, without a deployment or database.
    settings.configure()
    from app_core import agent_run_api_protocol as wire

    receipt = {"status": 200, "contentType": "application/json", "schema": wire.OperationReceipt.__name__}
    authorization = {"name": "Authorization", "required": True, "schema": {"type": "string"}}
    operations = [
        {
            "id": "createRun", "method": "POST", "path": _public_path(wire.CREATE_RUN_PATH),
            "requestSchema": wire.CreateRunRequest.__name__,
            "headers": [authorization, {"name": wire.IDEMPOTENCY_HEADER, "required": True, "schema": {"type": "string"}}],
            "responses": [{"status": 200, "contentType": "text/event-stream", "condition": "newRun"},
                          {**receipt, "condition": "existingOperation"}],
        },
        {
            "id": "getOperation", "method": "GET", "path": _public_path(wire.GET_OPERATION_PATH),
            "headers": [authorization],
            "responses": [{**receipt, "condition": "statusOnly"}],
        },
        {
            "id": "cancelRun", "method": "POST", "path": _public_path(wire.CANCEL_RUN_PATH),
            "headers": [authorization], "emptyBody": True,
            "responses": [{**receipt, "condition": "terminal"}, {
                "status": 202, "contentType": "application/json", "schema": wire.CancellationRequested.__name__,
                "condition": "cancelRequested",
            }],
        },
    ]
    types = (wire.CreateRunRequest, wire.CancellationRequested, *(event.schema for event in wire.EVENTS))
    return {
        "operations": operations,
        "schemas": {payload.__name__: TypeAdapter(payload).json_schema() for payload in types},
        "events": [{"name": event.name, "schema": event.schema.__name__} for event in wire.EVENTS],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = json.dumps(_reference(), ensure_ascii=False, indent=2) + "\n"
    if args.check:
        if not OUTPUT.is_file() or OUTPUT.read_text(encoding="utf-8") != generated:
            raise SystemExit(f"{OUTPUT.name} is stale; run python scripts/workspace/agent-run-api-reference.py")
    else:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(generated, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
