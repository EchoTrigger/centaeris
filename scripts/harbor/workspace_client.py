"""Bounded, cookie-authenticated client for the existing public Workspace API.

This controller-side client never executes tools or contacts Runtime directly.
Mutation uncertainty is reconciled by the existing durable operation receipt.
"""
from __future__ import annotations

import http.client
import http.cookiejar
import json
import math
import re
import socket
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPCookieProcessor, HTTPRedirectHandler, ProxyHandler, Request, build_opener


class WorkspaceError(RuntimeError):
    def __init__(self, code, status=None):
        self.code, self.status = code, status
        super().__init__(f"Workspace {code}" + (f" (HTTP {status})" if status else ""))


class SubmissionUncertain(WorkspaceError):
    """No new operation identity may be allocated after this exception."""


class _NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        raise WorkspaceError("redirect_rejected", code)


def _segment(value):
    if not isinstance(value, str) or not value or len(value) > 160:
        raise ValueError("invalid API identity")
    return quote(value, safe="")


class WorkspaceClient:
    def __init__(self, base_url, *, request_timeout=10, max_event_bytes=8 * 1024 * 1024):
        parsed = urlsplit(base_url)
        if (parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username
                or parsed.password or parsed.path not in {"", "/"} or parsed.query or parsed.fragment):
            raise ValueError("Workspace URL must be an origin without credentials")
        if parsed.scheme != "https" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}:
            raise ValueError("Workspace credentials require HTTPS or a loopback SSH tunnel")
        if (type(request_timeout) not in {int, float} or not math.isfinite(request_timeout)
                or not 0 < request_timeout <= 30):
            raise ValueError("request_timeout must be between zero and 30 seconds")
        if type(max_event_bytes) is not int or not 1024 <= max_event_bytes <= 16 * 1024 * 1024:
            raise ValueError("max_event_bytes must be between 1 KiB and 16 MiB")
        self.base_url = base_url.rstrip("/")
        self.request_timeout = request_timeout
        self.max_event_bytes = max_event_bytes
        self.cookies = http.cookiejar.CookieJar()
        # The control-plane connection must not inherit a model/provider proxy.
        self.opener = build_opener(ProxyHandler({}), _NoRedirects(), HTTPCookieProcessor(self.cookies))
        self.csrf = None

    def _request(self, method, path, payload=None, *, headers=None):
        request_headers = {"Accept": "application/json", **(headers or {})}
        if payload is not None:
            request_headers["Content-Type"] = "application/json"
        if method not in {"GET", "HEAD"} and self.csrf:
            request_headers.update({"X-CSRFToken": self.csrf, "Origin": self.base_url,
                                   "Referer": self.base_url + "/"})
        body = json.dumps(payload, ensure_ascii=False).encode() if payload is not None else None
        return Request(self.base_url + path, data=body, method=method, headers=request_headers)

    def _json(self, method, path, payload=None):
        try:
            with self.opener.open(self._request(method, path, payload), timeout=self.request_timeout) as response:
                raw = response.read(1024 * 1024 + 1)
                if len(raw) > 1024 * 1024:
                    raise WorkspaceError("response_too_large")
                value = json.loads(raw)
                if not isinstance(value, dict):
                    raise WorkspaceError("response_invalid")
                return value
        except HTTPError as error:
            # Do not include arbitrary server bodies, URLs, or credentials in diagnostics.
            with error:
                try:
                    value = json.loads(error.read(4096))
                    code = value.get("error") if isinstance(value, dict) else None
                except (ValueError, UnicodeError):
                    code = None
            if not isinstance(code, str) or re.fullmatch(r"[a-z][a-z0-9_]{0,127}", code) is None:
                code = "http_error"
            raise WorkspaceError(code, error.code) from None
        except (URLError, OSError, http.client.HTTPException):
            raise WorkspaceError("transport_unavailable") from None
        except (ValueError, UnicodeError):
            raise WorkspaceError("response_invalid") from None

    def login(self, email, password):
        self.csrf = self._json("GET", "/api/csrf").get("csrfToken")
        if not isinstance(self.csrf, str) or not self.csrf:
            raise WorkspaceError("csrf_response_invalid")
        result = self._json("POST", "/api/login", {"email": email, "password": password})
        # Django rotates its CSRF secret during login.
        self.csrf = self._json("GET", "/api/csrf").get("csrfToken")
        if not isinstance(result.get("user"), dict) or not isinstance(self.csrf, str) or not self.csrf:
            raise WorkspaceError("login_response_invalid")

    def operation(self, workspace_id, command, operation_id):
        if command not in {"createSession", "submitMessage"}:
            raise ValueError("unsupported operation")
        try:
            value = self._json("GET", f"/api/workspaces/{_segment(workspace_id)}/operations/{command}/{_segment(operation_id)}")
        except WorkspaceError as error:
            if error.status == 404 and error.code == "operation_not_found":
                return None
            raise
        return self._receipt(value, command, operation_id)

    @staticmethod
    def _receipt(value, command, operation_id):
        if (set(value) != {"operationId", "command", "status", "sessionId", "agentRunId", "turnId"}
                or value["operationId"] != operation_id or value["command"] != command
                or value["status"] != "accepted" or not isinstance(value["sessionId"], str) or not value["sessionId"]
                or (command == "submitMessage" and not all(isinstance(value[k], str) and value[k] for k in ("agentRunId", "turnId")))
                or (command == "createSession" and (value["agentRunId"] is not None or value["turnId"] is not None))):
            raise WorkspaceError("operation_receipt_invalid")
        return value

    def _mutate(self, workspace_id, command, path, payload):
        operation_id = payload["operationId"]
        if not isinstance(operation_id, str) or re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", operation_id) is None:
            raise ValueError("invalid operation identity")
        try:
            value = self._json("POST", path, payload)
        except WorkspaceError as primary:
            if primary.code != "transport_unavailable" and (primary.status is None or primary.status < 500):
                raise
            try:
                value = self.operation(workspace_id, command, operation_id)
            except WorkspaceError:
                value = None
            if value is None:
                raise SubmissionUncertain("submission_uncertain") from None
        return self._receipt(value, command, operation_id)

    def create_session(self, workspace_id, agent_id, operation_id, *, project_id=None):
        return self._mutate(workspace_id, "createSession", f"/api/workspaces/{_segment(workspace_id)}/sessions",
                            {"operationId": operation_id, "agentId": agent_id, "projectId": project_id})

    def submit_message(self, workspace_id, session_id, agent_id, operation_id, text, model_config_ref, thinking_mode):
        value = self._mutate(workspace_id, "submitMessage",
            f"/api/workspaces/{_segment(workspace_id)}/sessions/{_segment(session_id)}/messages",
            {"operationId": operation_id, "text": text, "agentId": agent_id,
             "modelConfigRef": model_config_ref, "thinkingMode": thinking_mode, "attachmentRefs": []})
        if value["sessionId"] != session_id:
            raise WorkspaceError("operation_session_mismatch")
        return value

    def session(self, session_id):
        return self._json("GET", f"/api/sessions/{_segment(session_id)}")

    def run_environment(self, session_id, agent_run_id):
        return self._json("GET", f"/api/sessions/{_segment(session_id)}/agent-runs/{_segment(agent_run_id)}/environment")

    def run_fact(self, session_id, agent_run_id):
        """Read the bound Run fact, including terminal facts with no event stream."""
        value = self._json("GET", f"/api/sessions/{_segment(session_id)}/preview?limit=1")
        if value.get("schema") != "session.preview.v1" or value.get("sessionId") != session_id:
            raise WorkspaceError("run_fact_identity_invalid")
        if "runFact" not in value:
            raise WorkspaceError("run_fact_response_invalid")
        fact = value["runFact"]
        if fact is None:
            return None
        if not isinstance(fact, dict) or fact.get("agentRunId") != agent_run_id:
            raise WorkspaceError("run_fact_identity_invalid")
        if (set(fact) != {"agentRunId", "status", "sourceType", "eventId", "createdAtMs"}
                or type(fact["createdAtMs"]) is not int or fact["createdAtMs"] < 0):
            raise WorkspaceError("run_fact_response_invalid")
        status, source, event = fact["status"], fact["sourceType"], fact["eventId"]
        if not ((status in {"queued", "running"} and source == "hostedRun" and event is None)
                or (status in {"completed", "failed", "cancelled"} and source == "sessionEvent"
                    and isinstance(event, str) and bool(event))
                or (status == "failed" and source == "preAdmissionFailure" and event is None)
                or (status == "cancelled" and source == "preAdmissionCancellation" and event is None)):
            raise WorkspaceError("run_fact_response_invalid")
        return fact

    def cancel_run(self, session_id, agent_run_id):
        value = self._json("POST", f"/api/sessions/{_segment(session_id)}/agent-runs/{_segment(agent_run_id)}/cancel", {})
        if (value.get("agentRunId") != agent_run_id or value.get("status") not in {"queued", "running", "completed", "failed", "cancelled"}
                or value.get("disposition") not in {"requested", "terminal"}):
            raise WorkspaceError("cancellation_response_invalid")
        return value

    def read_run_events(self, session_id, agent_run_id, cursor="0-0", *, after_sequence=0):
        """Read one bounded SSE slice; only committed facts advance the cursor.

        The caller persists each returned cursor before reconnecting. Live text is
        discarded and never interpreted as completion or reward.
        """
        result = {"cursor": cursor, "sourceSequence": after_sequence, "status": "running"}
        path = f"/api/sessions/{_segment(session_id)}/agent-runs/{_segment(agent_run_id)}/events"
        request = self._request("GET", path, headers={"Accept": "text/event-stream", "Last-Event-ID": cursor})
        end = time.monotonic() + self.request_timeout
        try:
            with self.opener.open(request, timeout=self.request_timeout) as response:
                if response.headers.get_content_type() != "text/event-stream":
                    raise WorkspaceError("stream_content_type_invalid")
                data, event_cursor, size, count = [], None, 0, 0
                while time.monotonic() < end:
                    line = response.readline(self.max_event_bytes + 1)
                    if not line:
                        break
                    size += len(line)
                    if size > self.max_event_bytes:
                        raise WorkspaceError("stream_event_too_large")
                    text = line.decode("utf-8").rstrip("\r\n")
                    if text:
                        if text.startswith("data:"):
                            data.append(text[5:].removeprefix(" "))
                        elif text.startswith("id:"):
                            event_cursor = text[3:].removeprefix(" ")
                        continue
                    if data:
                        item = json.loads("\n".join(data))
                        self._apply_stream_item(result, item, event_cursor, session_id, agent_run_id)
                        count += 1
                        if result["status"] != "running" or count >= 256:
                            return result
                    data, event_cursor, size = [], None, 0
        except HTTPError as error:
            status = error.code
            error.close()
            raise WorkspaceError("stream_http_error", status) from None
        except (TimeoutError, socket.timeout):
            pass
        except (URLError, OSError, http.client.HTTPException):
            raise WorkspaceError("transport_unavailable") from None
        except (ValueError, UnicodeError):
            raise WorkspaceError("stream_response_invalid") from None
        return result

    @staticmethod
    def _apply_stream_item(result, item, cursor, session_id, agent_run_id):
        if (not isinstance(item, dict) or item.get("schema") != "session.stream.item.v1"
                or item.get("agentRunId") != agent_run_id or item.get("kind") not in {"live", "committed"}):
            raise WorkspaceError("stream_identity_invalid")
        if item["kind"] == "live":
            return
        event, sequence = item.get("event"), item.get("sourceSequence")
        if (type(sequence) is not int or sequence <= result["sourceSequence"]
                or not isinstance(cursor, str) or not cursor or len(cursor) > 512
                or not isinstance(event, dict) or event.get("sequence") != sequence
                or event.get("sessionId") != session_id or event.get("agentRunId") != agent_run_id):
            raise WorkspaceError("stream_identity_invalid")
        result.update(cursor=cursor, sourceSequence=sequence)
        terminal = {"agent_run_completed": "completed", "agent_run_failed": "failed", "agent_run_interrupted": "cancelled"}
        result["status"] = terminal.get(event.get("type"), "running")
