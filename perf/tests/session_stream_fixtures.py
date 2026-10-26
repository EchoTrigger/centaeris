"""Generate test stream items with the real API's pure committed-item producer."""
import ast
import json
from pathlib import Path
import sys
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
source = ROOT / 'packages/api/app_core/agent_run_stream.py'
tree = ast.parse(source.read_text(encoding='utf-8'))
selected = [node for node in tree.body if (
    isinstance(node, ast.FunctionDef) and node.name == '_committed_item') or (
    isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == 'STREAM_ITEM_SCHEMA' for target in node.targets))]
namespace = {'AgentRun': object}
exec(compile(ast.Module(body=selected, type_ignores=[]), str(source), 'exec'), namespace)
values = json.load(sys.stdin)
items = []
for event in values:
    run = SimpleNamespace(id=event['agentRunId'], session_id=event['sessionId'])
    items.append(namespace['_committed_item'](run, {'sequence': event['sequence'], 'eventId': event['eventId'], 'payload': event}))
print(json.dumps(items))
