import importlib.util
import io
import json
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location("experience_mock", Path(__file__).resolve().parents[1] / "mock-model/server.py")
MARKER = "PERF_EXPERIENCE_" + "a" * 32


class LightExperienceMockTests(unittest.TestCase):
    def request(self, prompt, rounds=0, previous=True):
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        handler = object.__new__(module.Handler)
        handler.path = "/v1/responses"
        items = ([{"role": "user", "content": "old"}, {"type": "function_call_output", "output": "old"}] if previous else [])
        items += [{"role": "user", "content": prompt}]
        items += [{"type": "function_call_output", "output": "ok"} for _ in range(rounds)]
        body = json.dumps({"model": "perf-light-experience", "stream": True, "input": items}).encode()
        handler.headers = {"content-length": str(len(body))}
        handler.rfile = io.BytesIO(body)
        handler.wfile = io.BytesIO()
        statuses = []
        handler.send_response = statuses.append
        handler.send_error = lambda status, *args: statuses.append(status)
        handler.send_header = lambda *args: None
        handler.end_headers = lambda: None
        handler.do_POST()
        events = [json.loads(line[6:]) for line in handler.wfile.getvalue().decode().splitlines() if line.startswith("data: ")]
        return statuses, events[-1]["response"]["output"] if events else []

    def test_text_is_tool_free_and_ignores_previous_session_tool_rounds(self):
        final = MARKER + "_TEXT_OK"
        statuses, output = self.request("Synthetic performance task. Do not call any tools. Reply with exactly " + final)
        self.assertEqual(statuses, [200])
        self.assertEqual(output[0]["type"], "message")
        self.assertEqual(output[0]["content"][0]["text"], final)

    def test_commands_are_sequential_and_latest_user_resets_rounds(self):
        commands = ["printf 'created'", "printf '42'"]
        final = MARKER + "_MULTI_OK:42"
        prompt = ("Synthetic performance task. Invoke bash exactly 2 time(s), sequentially, with the following exact command(s). "
                  "Do not combine commands, add other tool calls, or publish artifacts. Use timeout_ms=10000 for each bash call. "
                  + "\n".join(f"Command {i + 1}:\n{value}" for i, value in enumerate(commands))
                  + "\nAfter checking the tool outputs, reply with exactly " + final)
        for rounds, command in enumerate(commands):
            _, output = self.request(prompt, rounds)
            self.assertEqual(output[0]["name"], "bash")
            self.assertEqual(json.loads(output[0]["arguments"]), {"command": command, "timeout_ms": 10000})
        _, output = self.request(prompt, 2)
        self.assertEqual(output[0]["content"][0]["text"], final)

    def test_unknown_prompt_refused(self):
        statuses, output = self.request("normal user prompt")
        self.assertEqual(statuses, [422])
        self.assertEqual(output, [])

    def test_single_tool_finishes_after_one_command(self):
        final = MARKER + "_TOOL_OK"
        command = "printf '" + final + "\\n'"
        prompt = ("Synthetic performance task. Invoke bash exactly 1 time(s), sequentially, with the following exact command(s). "
                  "Do not combine commands, add other tool calls, or publish artifacts. Use timeout_ms=10000 for each bash call. "
                  "Command 1:\n" + command + "\nAfter checking the tool outputs, reply with exactly " + final)
        _, output = self.request(prompt)
        self.assertEqual(json.loads(output[0]["arguments"]), {"command": command, "timeout_ms": 10000})
        _, output = self.request(prompt, 1)
        self.assertEqual(output[0]["content"][0]["text"], final)


if __name__ == "__main__":
    unittest.main()
